// 릴리스 빌드에서 콘솔 창 숨김
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    band_lib::run();
}
