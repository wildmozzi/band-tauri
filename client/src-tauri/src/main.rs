// 디버깅용: 콘솔 창 표시 (나중에 다시 숨길 수 있음)
// #![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    println!("[MAIN] Starting...");
    band_lib::run();
    println!("[MAIN] Exited");
}
