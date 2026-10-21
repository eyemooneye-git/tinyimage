//! 原生 Win32 小窗口。
//!
//! 没有使用任何 GUI 框架，整个界面用 GDI 自绘：
//! - 拖拽 / 点击选择图片
//! - 后台线程压缩，窗口不卡死
//! - 完成后可直接打开所在文件夹

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreatePen,
    CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, InvalidateRect,
    RoundRect, ScreenToClient, SelectObject, SetBkMode, SetTextColor, UpdateWindow,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DRAW_TEXT_FORMAT, DT_CENTER,
    DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK,
    HBRUSH, HDC, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS, PS_SOLID, SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST,
    OPENFILENAMEW,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, GetDpiForWindow, GetSystemMetricsForDpi, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::Shell::{
    DragAcceptFiles, DragFinish, DragQueryFileW, ShellExecuteW, HDROP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow,
    DispatchMessageW, GetClientRect, GetCursorPos, GetMessageW, GetSystemMetrics,
    GetWindowLongPtrW, LoadCursorW, LoadIconW, LoadImageW, MessageBoxW, PostMessageW,
    PostQuitMessage, RegisterClassExW, SendMessageW, SetCursor, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HCURSOR, HICON, HTCLIENT,
    ICON_BIG, ICON_SMALL, IDC_ARROW, IDC_HAND, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTCOLOR,
    MB_ICONERROR, MB_OK, MSG, SM_CXICON, SM_CXSCREEN, SM_CXSMICON, SM_CYSCREEN, SWP_NOACTIVATE,
    SWP_NOZORDER, SW_SHOW, SW_SHOWNORMAL, WINDOW_EX_STYLE, WM_APP, WM_CAPTURECHANGED, WM_CLOSE,
    WM_CREATE, WM_DESTROY, WM_DPICHANGED, WM_DROPFILES, WM_ERASEBKGND, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCDESTROY, WM_PAINT, WM_SETCURSOR, WM_SETICON, WM_SIZE,
    WNDCLASSEXW, WS_CAPTION, WS_CLIPCHILDREN, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
};

use crate::compress::{self, CompressionResult};

const APP_CLASS: PCWSTR = w!("TinyImageWindow");
const WM_APP_DONE: u32 = WM_APP + 1;

const BASE_WIDTH: f32 = 660.0;
const BASE_HEIGHT: f32 = 480.0;

static WORKER_RESULT: Mutex<Option<Result<CompressionResult, String>>> = Mutex::new(None);

// ---------------------------------------------------------------------------
// 颜色与字体
// ---------------------------------------------------------------------------

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

const BG: COLORREF = rgb(247, 247, 245);
const CARD: COLORREF = rgb(255, 255, 255);
const TEXT: COLORREF = rgb(24, 24, 27);
const MUTED: COLORREF = rgb(113, 113, 122);
const BORDER: COLORREF = rgb(220, 220, 215);
const ACCENT: COLORREF = rgb(37, 99, 235);
const ACCENT_HOVER: COLORREF = rgb(29, 78, 216);
const ACCENT_DARK: COLORREF = rgb(30, 64, 175);
const ACCENT_SOFT: COLORREF = rgb(239, 246, 255);
const SUCCESS: COLORREF = rgb(21, 128, 61);
const ERROR: COLORREF = rgb(220, 38, 38);
const WHITE: COLORREF = rgb(255, 255, 255);
const DISABLED_BG: COLORREF = rgb(235, 235, 232);
const DISABLED_TEXT: COLORREF = rgb(161, 161, 170);
const SOFT_HOVER: COLORREF = rgb(245, 245, 242);
const SOFT_PRESSED: COLORREF = rgb(235, 235, 232);
const SOFT_BORDER_HOVER: COLORREF = rgb(200, 200, 195);

struct Fonts {
    title: HFONT,
    subtitle: HFONT,
    drop: HFONT,
    drop_sub: HFONT,
    body: HFONT,
    result: HFONT,
    button: HFONT,
    footer: HFONT,
}

impl Fonts {
    fn new(scale: f32) -> Self {
        Self {
            title: make_font(27.0, 600, scale),
            subtitle: make_font(13.0, 400, scale),
            drop: make_font(17.0, 600, scale),
            drop_sub: make_font(12.0, 400, scale),
            body: make_font(13.0, 400, scale),
            result: make_font(17.0, 600, scale),
            button: make_font(14.0, 600, scale),
            footer: make_font(11.0, 400, scale),
        }
    }

    fn delete(&self) {
        unsafe {
            for font in [
                self.title,
                self.subtitle,
                self.drop,
                self.drop_sub,
                self.body,
                self.result,
                self.button,
                self.footer,
            ] {
                if !font.is_invalid() {
                    let _ = DeleteObject(HGDIOBJ(font.0));
                }
            }
        }
    }
}

fn make_font(size: f32, weight: i32, scale: f32) -> HFONT {
    unsafe {
        CreateFontW(
            -((size * scale).round() as i32).max(1),
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Microsoft YaHei UI"),
        )
    }
}

// ---------------------------------------------------------------------------
// 状态
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    None,
    Drop,
    Choose,
    Open,
}

struct Ui {
    dpi: u32,
    scale: f32,
    fonts: Fonts,
    bg_brush: HBRUSH,
    arrow_cursor: HCURSOR,
    hand_cursor: HCURSOR,
    client_w: i32,
    client_h: i32,
    drop_rect: RECT,
    choose_rect: RECT,
    open_rect: RECT,
    hover: Hit,
    pressed: Option<Hit>,
    busy: bool,
    output: Option<PathBuf>,
    subtitle: String,
    file_line: String,
    result_line: String,
    detail_line: String,
    result_color: COLORREF,
}

impl Ui {
    unsafe fn new(hwnd: HWND, dpi: u32) -> Self {
        let scale = dpi as f32 / 96.0;
        let mut ui = Self {
            dpi,
            scale,
            fonts: Fonts::new(scale),
            bg_brush: CreateSolidBrush(BG),
            arrow_cursor: LoadCursorW(None, IDC_ARROW).unwrap_or(HCURSOR(std::ptr::null_mut())),
            hand_cursor: LoadCursorW(None, IDC_HAND).unwrap_or(HCURSOR(std::ptr::null_mut())),
            client_w: 0,
            client_h: 0,
            drop_rect: RECT::default(),
            choose_rect: RECT::default(),
            open_rect: RECT::default(),
            hover: Hit::None,
            pressed: None,
            busy: false,
            output: None,
            subtitle: format!(
                "PNG / JPG 图片 · 目标 {} KB · 优先保留清晰度",
                compress::TARGET_BYTES / 1024
            ),
            file_line: "尚未选择图片".to_string(),
            result_line: "拖入图片，几秒完成".to_string(),
            detail_line: "支持 PNG / JPG / JPEG · 一次一张 · 原图自动备份".to_string(),
            result_color: TEXT,
        };
        ui.relayout(hwnd);
        ui
    }

    unsafe fn relayout(&mut self, hwnd: HWND) {
        let mut rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut rect);
        self.client_w = rect.right.max(0);
        self.client_h = rect.bottom.max(0);

        let s = self.scale;
        let padding = sc(30.0, s);
        let right = (self.client_w - padding).max(padding + sc(100.0, s));

        self.drop_rect = RECT {
            left: padding,
            top: sc(96.0, s),
            right,
            bottom: sc(246.0, s),
        };
        self.choose_rect = RECT {
            left: padding,
            top: sc(396.0, s),
            right: padding + sc(160.0, s),
            bottom: sc(436.0, s),
        };
        self.open_rect = RECT {
            left: padding + sc(172.0, s),
            top: sc(396.0, s),
            right: padding + sc(342.0, s),
            bottom: sc(436.0, s),
        };
    }

    unsafe fn set_dpi(&mut self, hwnd: HWND, dpi: u32) {
        if dpi == 0 || dpi == self.dpi {
            return;
        }
        self.dpi = dpi;
        self.scale = dpi as f32 / 96.0;
        self.fonts.delete();
        self.fonts = Fonts::new(self.scale);
        self.relayout(hwnd);
    }

    fn hit_test(&self, x: i32, y: i32) -> Hit {
        if self.busy {
            return Hit::None;
        }
        if point_in(self.choose_rect, x, y) {
            Hit::Choose
        } else if self.output.is_some() && point_in(self.open_rect, x, y) {
            Hit::Open
        } else if point_in(self.drop_rect, x, y) {
            Hit::Drop
        } else {
            Hit::None
        }
    }

    unsafe fn paint(&self, hdc: HDC) {
        let background = RECT {
            left: 0,
            top: 0,
            right: self.client_w,
            bottom: self.client_h,
        };
        FillRect(hdc, &background, self.bg_brush);

        let s = self.scale;
        let padding = sc(30.0, s);
        let right = (self.client_w - padding).max(padding + sc(100.0, s));

        draw_text(
            hdc,
            "tinyimage",
            RECT {
                left: padding,
                top: sc(16.0, s),
                right: right,
                bottom: sc(56.0, s),
            },
            self.fonts.title,
            TEXT,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
        );
        draw_text(
            hdc,
            &self.subtitle,
            RECT {
                left: padding + sc(2.0, s),
                top: sc(56.0, s),
                right: right,
                bottom: sc(80.0, s),
            },
            self.fonts.subtitle,
            MUTED,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
        );

        let drop_hovered = !self.busy && self.hover == Hit::Drop;
        round_rect(
            hdc,
            self.drop_rect,
            sc(14.0, s),
            if drop_hovered { ACCENT_SOFT } else { CARD },
            if drop_hovered { ACCENT } else { BORDER },
            if drop_hovered { sc(2.0, s).max(2) } else { 1 },
        );

        draw_text(
            hdc,
            "↓",
            RECT {
                left: self.drop_rect.left,
                top: sc(106.0, s),
                right: self.drop_rect.right,
                bottom: sc(140.0, s),
            },
            self.fonts.drop,
            ACCENT,
            DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
        );
        draw_text(
            hdc,
            "把 PNG / JPG 拖到这里",
            RECT {
                left: self.drop_rect.left,
                top: sc(144.0, s),
                right: self.drop_rect.right,
                bottom: sc(174.0, s),
            },
            self.fonts.drop,
            TEXT,
            DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
        );
        draw_text(
            hdc,
            "或点击下方「选择图片」",
            RECT {
                left: self.drop_rect.left,
                top: sc(180.0, s),
                right: self.drop_rect.right,
                bottom: sc(204.0, s),
            },
            self.fonts.drop_sub,
            MUTED,
            DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
        );

        draw_text(
            hdc,
            &self.file_line,
            RECT {
                left: padding,
                top: sc(258.0, s),
                right,
                bottom: sc(282.0, s),
            },
            self.fonts.body,
            MUTED,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        draw_text(
            hdc,
            &self.result_line,
            RECT {
                left: padding,
                top: sc(288.0, s),
                right,
                bottom: sc(318.0, s),
            },
            self.fonts.result,
            self.result_color,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        draw_text(
            hdc,
            &self.detail_line,
            RECT {
                left: padding,
                top: sc(324.0, s),
                right,
                bottom: sc(380.0, s),
            },
            self.fonts.body,
            MUTED,
            DT_LEFT | DT_WORDBREAK | DT_NOPREFIX,
        );

        self.paint_button(
            hdc,
            self.choose_rect,
            "选择图片…",
            Hit::Choose,
            true,
            !self.busy,
        );
        self.paint_button(
            hdc,
            self.open_rect,
            "打开所在文件夹",
            Hit::Open,
            false,
            !self.busy && self.output.is_some(),
        );

        draw_text(
            hdc,
            "原图自动备份，不会丢失",
            RECT {
                left: right - sc(250.0, s),
                top: sc(406.0, s),
                right,
                bottom: sc(428.0, s),
            },
            self.fonts.footer,
            MUTED,
            DT_RIGHT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
        );
    }

    fn paint_button(
        &self,
        hdc: HDC,
        rect: RECT,
        label: &str,
        hit: Hit,
        primary: bool,
        enabled: bool,
    ) {
        let s = self.scale;
        let hovered = enabled && self.hover == hit;
        let pressed = hovered && self.pressed == Some(hit);

        let (background, foreground, border) = if !enabled {
            (DISABLED_BG, DISABLED_TEXT, DISABLED_BG)
        } else if primary {
            let bg = if pressed {
                ACCENT_DARK
            } else if hovered {
                ACCENT_HOVER
            } else {
                ACCENT
            };
            (bg, WHITE, bg)
        } else {
            let bg = if pressed {
                SOFT_PRESSED
            } else if hovered {
                SOFT_HOVER
            } else {
                CARD
            };
            let border = if hovered { SOFT_BORDER_HOVER } else { BORDER };
            (bg, TEXT, border)
        };

        round_rect(
            hdc,
            rect,
            sc(10.0, s),
            background,
            border,
            sc(1.0, s).max(1),
        );

        let text_rect = if pressed {
            RECT {
                left: rect.left,
                top: rect.top + sc(1.0, s),
                right: rect.right,
                bottom: rect.bottom + sc(1.0, s),
            }
        } else {
            rect
        };

        draw_text(
            hdc,
            label,
            text_rect,
            self.fonts.button,
            foreground,
            DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
        );
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        self.fonts.delete();
        unsafe {
            if !self.bg_brush.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.bg_brush.0));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 绘图工具
// ---------------------------------------------------------------------------

fn sc(value: f32, scale: f32) -> i32 {
    (value * scale).round() as i32
}

fn point_in(rect: RECT, x: i32, y: i32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

fn draw_text(
    hdc: HDC,
    text: &str,
    mut rect: RECT,
    font: HFONT,
    color: COLORREF,
    format: DRAW_TEXT_FORMAT,
) {
    if text.is_empty() || font.is_invalid() {
        return;
    }
    unsafe {
        let old_font = SelectObject(hdc, HGDIOBJ(font.0));
        SetTextColor(hdc, color);
        SetBkMode(hdc, TRANSPARENT);
        let mut buffer: Vec<u16> = text.encode_utf16().collect();
        DrawTextW(hdc, &mut buffer, &mut rect, format);
        SelectObject(hdc, old_font);
    }
}

fn round_rect(
    hdc: HDC,
    rect: RECT,
    radius: i32,
    background: COLORREF,
    border: COLORREF,
    border_width: i32,
) {
    unsafe {
        let brush = CreateSolidBrush(background);
        let pen = CreatePen(PS_SOLID, border_width.max(1), border);
        if brush.is_invalid() || pen.is_invalid() {
            return;
        }

        let old_brush = SelectObject(hdc, HGDIOBJ(brush.0));
        let old_pen = SelectObject(hdc, HGDIOBJ(pen.0));
        let _ = RoundRect(
            hdc,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            radius * 2,
            radius * 2,
        );
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
        let _ = DeleteObject(HGDIOBJ(pen.0));
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
}

// ---------------------------------------------------------------------------
// 压缩任务与文件操作
// ---------------------------------------------------------------------------

unsafe fn start_compress(hwnd: HWND, ui: &mut Ui, path: PathBuf) {
    if ui.busy {
        ui.detail_line = "正在处理上一张图片，请稍候…".to_string();
        let _ = InvalidateRect(Some(hwnd), None, false);
        return;
    }

    // 命令行传入相对路径时，转成绝对路径，方便完成后定位文件。
    let path = match std::path::absolute(&path) {
        Ok(path) => path,
        Err(_) => path,
    };

    let name = file_name(&path);
    let size = std::fs::metadata(&path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);

    ui.busy = true;
    ui.hover = Hit::None;
    ui.pressed = None;
    ui.output = None;
    ui.file_line = format!("{name} · {}", format_bytes(size));
    ui.result_line = "正在压缩…".to_string();
    ui.detail_line = "正在分析画质与体积，请稍候…".to_string();
    ui.result_color = ACCENT;
    let _ = InvalidateRect(Some(hwnd), None, false);

    let raw_hwnd = hwnd.0 as isize;
    std::thread::spawn(move || {
        let result = compress::compress_file(&path);
        if let Ok(mut slot) = WORKER_RESULT.lock() {
            *slot = Some(result);
        }
        let hwnd = HWND(raw_hwnd as *mut std::ffi::c_void);
        let _ = PostMessageW(Some(hwnd), WM_APP_DONE, WPARAM(0), LPARAM(0));
    });
}

unsafe fn finish_compress(hwnd: HWND, ui: &mut Ui) {
    ui.busy = false;
    ui.pressed = None;
    ui.hover = Hit::None;

    let result = WORKER_RESULT.lock().ok().and_then(|mut slot| slot.take());

    match result {
        Some(Ok(result)) => {
            let name = file_name(&result.path);
            ui.output = Some(result.path.clone());
            ui.file_line = format!(
                "{name} · {} · {}×{}",
                format_bytes(result.original_bytes),
                result.original_width,
                result.original_height
            );

            if result.skipped {
                ui.result_line = "无需压缩".to_string();
                ui.detail_line = format!(
                    "{}×{} · 原图已经在 65 KB 以内，未修改任何文件",
                    result.output_width, result.output_height
                );
                ui.result_color = SUCCESS;
            } else {
                ui.result_line = format!(
                    "完成  {} → {}",
                    format_bytes(result.original_bytes),
                    format_bytes(result.output_bytes)
                );
                let backup = result
                    .backup
                    .as_ref()
                    .map(|path| file_name(path))
                    .unwrap_or_else(|| "—".to_string());
                ui.detail_line = format!(
                    "{}×{} · {} · 节省 {:.1}%\n原图已备份为 {}",
                    result.output_width,
                    result.output_height,
                    result.note,
                    result.saved_percent(),
                    backup
                );
                ui.result_color = SUCCESS;
            }
        }
        Some(Err(error)) => {
            ui.output = None;
            ui.result_line = "压缩失败".to_string();
            ui.detail_line = error;
            ui.result_color = ERROR;
        }
        None => {
            ui.output = None;
            ui.result_line = "压缩失败".to_string();
            ui.detail_line = "后台任务没有返回结果，请重试".to_string();
            ui.result_color = ERROR;
        }
    }

    let _ = InvalidateRect(Some(hwnd), None, false);
}

unsafe fn pick_file(hwnd: HWND, ui: &mut Ui) {
    if ui.busy {
        return;
    }

    let filter: Vec<u16> =
        "图片文件 (*.png;*.jpg;*.jpeg)\0*.png;*.jpg;*.jpeg\0所有文件 (*.*)\0*.*\0\0"
            .encode_utf16()
            .collect();
    let mut buffer = vec![0u16; 32768];

    let mut dialog: OPENFILENAMEW = std::mem::zeroed();
    dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    dialog.hwndOwner = hwnd;
    dialog.lpstrFilter = PCWSTR(filter.as_ptr());
    dialog.lpstrFile = PWSTR(buffer.as_mut_ptr());
    dialog.nMaxFile = buffer.len() as u32;
    dialog.lpstrTitle = w!("选择要压缩的图片");
    dialog.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_EXPLORER | OFN_NOCHANGEDIR;

    if GetOpenFileNameW(&mut dialog).as_bool() {
        let length = buffer
            .iter()
            .position(|&value| value == 0)
            .unwrap_or(buffer.len());
        if length > 0 {
            let path = PathBuf::from(String::from_utf16_lossy(&buffer[..length]));
            start_compress(hwnd, ui, path);
        }
    }
}

unsafe fn handle_drop(hwnd: HWND, ui: &mut Ui, wparam: WPARAM) {
    let hdrop = HDROP(wparam.0 as *mut std::ffi::c_void);
    let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, None);
    if count == 0 {
        DragFinish(hdrop);
        return;
    }

    let length = DragQueryFileW(hdrop, 0, None) as usize;
    let mut buffer = vec![0u16; length + 1];
    DragQueryFileW(hdrop, 0, Some(&mut buffer));
    DragFinish(hdrop);

    if length == 0 {
        return;
    }

    let path = PathBuf::from(String::from_utf16_lossy(&buffer[..length]));
    start_compress(hwnd, ui, path);
    if count > 1 {
        ui.detail_line = "一次只处理一张，已选择第一张，正在压缩…".to_string();
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

unsafe fn open_in_explorer(path: &Path) {
    let operation: Vec<u16> = "open\0".encode_utf16().collect();
    let executable: Vec<u16> = "explorer.exe\0".encode_utf16().collect();
    let parameters: Vec<u16> = format!("/select,\"{}\"", path.display())
        .encode_utf16()
        .chain(Some(0))
        .collect();

    ShellExecuteW(
        None,
        PCWSTR(operation.as_ptr()),
        PCWSTR(executable.as_ptr()),
        PCWSTR(parameters.as_ptr()),
        None,
        SW_SHOWNORMAL,
    );
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

// ---------------------------------------------------------------------------
// 窗口过程
// ---------------------------------------------------------------------------

unsafe fn ui_mut<'a>(hwnd: HWND) -> Option<&'a mut Ui> {
    let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Ui;
    if pointer.is_null() {
        None
    } else {
        Some(&mut *pointer)
    }
}

fn loword(value: isize) -> i32 {
    (value as u32 & 0xffff) as u16 as i16 as i32
}

fn hiword(value: isize) -> i32 {
    ((value as u32 >> 16) & 0xffff) as u16 as i16 as i32
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CREATE => {
            let dpi = GetDpiForWindow(hwnd);
            let ui = Box::new(Ui::new(hwnd, dpi));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(ui) as isize);
            DragAcceptFiles(hwnd, true);
            LRESULT(0)
        }
        WM_NCDESTROY => {
            let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Ui;
            if !pointer.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(pointer));
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_PAINT => {
            let mut paint = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut paint);
            if !hdc.is_invalid() {
                if let Some(ui) = ui_mut(hwnd) {
                    if ui.client_w <= 0 || ui.client_h <= 0 {
                        ui.relayout(hwnd);
                    }
                    if ui.client_w > 0 && ui.client_h > 0 {
                        let memory = CreateCompatibleDC(Some(hdc));
                        let bitmap = CreateCompatibleBitmap(hdc, ui.client_w, ui.client_h);
                        if !memory.is_invalid() && !bitmap.is_invalid() {
                            let old_bitmap = SelectObject(memory, HGDIOBJ(bitmap.0));
                            ui.paint(memory);
                            let _ = BitBlt(
                                hdc,
                                0,
                                0,
                                ui.client_w,
                                ui.client_h,
                                Some(memory),
                                0,
                                0,
                                SRCCOPY,
                            );
                            SelectObject(memory, old_bitmap);
                            let _ = DeleteObject(HGDIOBJ(bitmap.0));
                        }
                        if !memory.is_invalid() {
                            let _ = DeleteDC(memory);
                        }
                    }
                }
            }
            let _ = EndPaint(hwnd, &paint);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            if let Some(ui) = ui_mut(hwnd) {
                ui.relayout(hwnd);
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let dpi = (wparam.0 & 0xffff) as u32;
            if let Some(ui) = ui_mut(hwnd) {
                ui.set_dpi(hwnd, dpi);
            }
            let suggested = &*(lparam.0 as *const RECT);
            let _ = SetWindowPos(
                hwnd,
                None,
                suggested.left,
                suggested.top,
                suggested.right - suggested.left,
                suggested.bottom - suggested.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if let Some(ui) = ui_mut(hwnd) {
                let hit = ui.hit_test(loword(lparam.0), hiword(lparam.0));
                if ui.hover != hit {
                    ui.hover = hit;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut track);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if let Some(ui) = ui_mut(hwnd) {
                if ui.hover != Hit::None {
                    ui.hover = Hit::None;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            if let Some(ui) = ui_mut(hwnd) {
                let hit = ui.hit_test(loword(lparam.0), hiword(lparam.0));
                if hit != Hit::None {
                    ui.pressed = Some(hit);
                    SetCapture(hwnd);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(ui) = ui_mut(hwnd) {
                let hit = ui.hit_test(loword(lparam.0), hiword(lparam.0));
                let pressed = ui.pressed.take();
                let _ = ReleaseCapture();
                if pressed.is_some() && pressed == Some(hit) {
                    match hit {
                        Hit::Choose | Hit::Drop => pick_file(hwnd, ui),
                        Hit::Open => {
                            if let Some(path) = ui.output.clone() {
                                open_in_explorer(&path);
                            }
                        }
                        Hit::None => {}
                    }
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            if let Some(ui) = ui_mut(hwnd) {
                if ui.pressed.is_some() {
                    ui.pressed = None;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let hit_test = (lparam.0 as u32) & 0xffff;
            if hit_test == HTCLIENT {
                if let Some(ui) = ui_mut(hwnd) {
                    let mut point = POINT::default();
                    if GetCursorPos(&mut point).is_ok() {
                        let _ = ScreenToClient(hwnd, &mut point);
                        let cursor = if ui.hit_test(point.x, point.y) != Hit::None {
                            ui.hand_cursor
                        } else {
                            ui.arrow_cursor
                        };
                        SetCursor(Some(cursor));
                        return LRESULT(1);
                    }
                }
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_DROPFILES => {
            if let Some(ui) = ui_mut(hwnd) {
                if ui.busy {
                    ui.detail_line = "正在处理上一张图片，请稍候…".to_string();
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    DragFinish(HDROP(wparam.0 as *mut std::ffi::c_void));
                } else {
                    handle_drop(hwnd, ui, wparam);
                }
            } else {
                DragFinish(HDROP(wparam.0 as *mut std::ffi::c_void));
            }
            LRESULT(0)
        }
        WM_APP_DONE => {
            if let Some(ui) = ui_mut(hwnd) {
                finish_compress(hwnd, ui);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            if let Some(ui) = ui_mut(hwnd) {
                if ui.busy {
                    ui.detail_line = "正在处理图片，完成后才能关闭…".to_string();
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    return LRESULT(0);
                }
            }
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

// ---------------------------------------------------------------------------
// 图标资源
// ---------------------------------------------------------------------------

/// `app.rc` 中图标组的资源 ID；exe 图标和窗口图标都从这里加载。
const APP_ICON_ID: usize = 1;

struct WindowIcons {
    big: HICON,
    small: HICON,
    owned_big: bool,
    owned_small: bool,
}

impl WindowIcons {
    unsafe fn load(instance: HINSTANCE, dpi: u32) -> Self {
        let big_size = GetSystemMetricsForDpi(SM_CXICON, dpi).max(16);
        let small_size = GetSystemMetricsForDpi(SM_CXSMICON, dpi).max(16);
        let fallback = LoadIconW(None, IDI_APPLICATION).unwrap_or(HICON(std::ptr::null_mut()));
        let big_loaded = load_app_icon(instance, big_size);
        let small_loaded = load_app_icon(instance, small_size);

        // 大图标和小图标始终来自同一组资源，避免标题栏与任务栏不一致。
        let (big, small, owned_big, owned_small) = match (big_loaded, small_loaded) {
            (Some(big), Some(small)) => (big, small, true, true),
            (Some(big), None) => (big, big, true, false),
            (None, Some(small)) => (small, small, true, false),
            (None, None) => (fallback, fallback, false, false),
        };

        Self {
            big,
            small,
            owned_big,
            owned_small,
        }
    }

    unsafe fn install(&self, hwnd: HWND) {
        if !self.big.is_invalid() {
            SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_BIG as usize)),
                Some(LPARAM(self.big.0 as isize)),
            );
        }
        if !self.small.is_invalid() {
            SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_SMALL as usize)),
                Some(LPARAM(self.small.0 as isize)),
            );
        }
    }
}

impl Drop for WindowIcons {
    fn drop(&mut self) {
        unsafe {
            if self.owned_big && !self.big.is_invalid() {
                let _ = DestroyIcon(self.big);
            }
            if self.owned_small && !self.small.is_invalid() && self.small.0 != self.big.0 {
                let _ = DestroyIcon(self.small);
            }
        }
    }
}

unsafe fn load_app_icon(instance: HINSTANCE, size: i32) -> Option<HICON> {
    let handle = LoadImageW(
        Some(instance),
        PCWSTR(APP_ICON_ID as *const u16),
        IMAGE_ICON,
        size,
        size,
        LR_DEFAULTCOLOR,
    )
    .ok()?;
    (!handle.is_invalid()).then(|| HICON(handle.0))
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

pub fn run() -> Result<(), String> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let module =
            GetModuleHandleW(None).map_err(|error| format!("获取程序实例失败：{error}"))?;
        let instance = HINSTANCE(module.0);
        let cursor =
            LoadCursorW(None, IDC_ARROW).map_err(|error| format!("加载光标失败：{error}"))?;
        let dpi = GetDpiForSystem().max(96);
        let icons = WindowIcons::load(instance, dpi);

        let window_class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: icons.big,
            hCursor: cursor,
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: APP_CLASS,
            hIconSm: icons.small,
        };

        let _ = RegisterClassExW(&window_class);

        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
        let scale = dpi as f32 / 96.0;
        let client_width = (BASE_WIDTH * scale).round() as i32;
        let client_height = (BASE_HEIGHT * scale).round() as i32;

        let mut window_rect = RECT {
            left: 0,
            top: 0,
            right: client_width,
            bottom: client_height,
        };
        AdjustWindowRectEx(&mut window_rect, style, false, WINDOW_EX_STYLE(0))
            .map_err(|error| format!("计算窗口尺寸失败：{error}"))?;

        let width = window_rect.right - window_rect.left;
        let height = window_rect.bottom - window_rect.top;
        let x = (GetSystemMetrics(SM_CXSCREEN) - width) / 2;
        let y = (GetSystemMetrics(SM_CYSCREEN) - height) / 2;

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            APP_CLASS,
            w!("tinyimage · 图片压缩到 50 KB"),
            style,
            x,
            y,
            width,
            height,
            None,
            None,
            Some(instance),
            None,
        )
        .map_err(|error| format!("创建窗口失败：{error}"))?;

        icons.install(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);

        if let Some(argument) = std::env::args_os().nth(1) {
            if let Some(ui) = ui_mut(hwnd) {
                start_compress(hwnd, ui, PathBuf::from(argument));
            }
        }

        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, None, 0, 0);
            if result.0 == 0 || result.0 == -1 {
                break;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    Ok(())
}

pub fn show_error(message: &str) {
    let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            w!("tinyimage"),
            MB_OK | MB_ICONERROR,
        );
    }
}
