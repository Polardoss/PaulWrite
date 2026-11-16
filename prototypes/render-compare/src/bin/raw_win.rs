//! Mesure de référence : coût d'une fenêtre Win32 brute (sans winit).

use std::time::Instant;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, LoadCursorW, PeekMessageW,
    RegisterClassExW, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, IDC_ARROW,
    MSG, PM_REMOVE, SW_SHOW, WNDCLASSEXW, WS_OVERLAPPEDWINDOW,
};

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn main() {
    let t0 = Instant::now();
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.0;
    unsafe {
        let hinst = GetModuleHandleW(PCWSTR::null()).unwrap();
        let t = Instant::now();
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap(),
            lpszClassName: w!("raw_win"),
            ..Default::default()
        };
        RegisterClassExW(&class);
        let t_class = ms(t);
        let t = Instant::now();
        let hwnd = CreateWindowExW(
            Default::default(),
            w!("raw_win"),
            w!("fenêtre brute"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1280,
            800,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .unwrap();
        let t_create = ms(t);
        let t = Instant::now();
        let _ = ShowWindow(hwnd, SW_SHOW);
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let t_show = ms(t);
        println!(
            "fenêtre Win32 brute : RegisterClass {t_class:.1} ms | CreateWindowEx {t_create:.1} ms | Show+pompe {t_show:.1} ms | total depuis main {:.1} ms",
            t0.elapsed().as_secs_f64() * 1000.0
        );
        let _ = DestroyWindow(hwnd);
    }
}
