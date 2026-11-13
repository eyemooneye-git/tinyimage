@echo off
chcp 65001 >nul

echo ===================================
echo 清理 tinyimage 项目...
echo ===================================
cd /d "c:\Projects\tinyimage"
call cargo clean
echo.
echo tinyimage 清理完成！
echo.

