#![windows_subsystem = "windows"]

use std::env;
use std::fs;
use std::mem::{size_of, zeroed};
use std::path::PathBuf;
use std::ptr::null_mut;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM, HMODULE, POINT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{
    RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, REG_VALUE_TYPE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus,
};

static mut MEDIA_MANAGER: Option<GlobalSystemMediaTransportControlsSessionManager> = None;

const WM_TRAYICON: u32 = WM_USER + 1;
const TIMER_ID_PLAYBACK: usize = 1;

const ICON_ID_DEFAULT: u32 = 1;
const ICON_ID_PREV: u32 = 2;
const ICON_ID_PLAY: u32 = 3;
const ICON_ID_NEXT: u32 = 4;

const MENU_SHOW_PREV: u16 = 101;
const MENU_SHOW_PLAY: u16 = 102;
const MENU_SHOW_NEXT: u16 = 103;
const MENU_EXIT: u16 = 105;

#[derive(Clone, Copy, Default, PartialEq)]
struct AppSettings {
    show_prev: bool,
    show_play: bool,
    show_next: bool,
    dark_icons: bool,
}

static mut APP_SETTINGS: AppSettings = AppSettings {
    show_prev: false,
    show_play: false,
    show_next: false,
    dark_icons: false,
};
static mut MAIN_HWND: HWND = HWND(null_mut());
static mut H_MODULE: HMODULE = HMODULE(null_mut());
static mut IS_PLAYING: bool = false;
static mut LAST_THEME_DARK: bool = true;
static mut THEME_CHECK_COUNTER: u32 = 0;

fn main() {
    unsafe {
        H_MODULE = GetModuleHandleW(None).unwrap_or(HMODULE(null_mut()));
        APP_SETTINGS = load_settings();
        
        let system_uses_dark = !is_system_light_theme();
        LAST_THEME_DARK = system_uses_dark;
        APP_SETTINGS.dark_icons = !system_uses_dark;

        let class_name = w!("ClickPlayClass");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: H_MODULE.into(),
            lpszClassName: class_name,
            ..zeroed()
        };

        RegisterClassExW(&wc);

        MAIN_HWND = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("ClickPlay"),
            WS_OVERLAPPED,
            0, 0, 0, 0,
            None,
            None,
            H_MODULE,
            None,
        ).unwrap_or(HWND(null_mut()));

        // Initialize media session manager once
        MEDIA_MANAGER = init_media_manager();

        IS_PLAYING = check_media_playing();
        update_tray_icons();

        // Single timer at 500ms handles both playback and theme checks
        SetTimer(MAIN_HWND, TIMER_ID_PLAYBACK, 500, None);

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let _ = KillTimer(MAIN_HWND, TIMER_ID_PLAYBACK);
        MEDIA_MANAGER = None;
        remove_all_icons();
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TIMER => {
            let timer_id = wparam.0;
            if timer_id == TIMER_ID_PLAYBACK {
                // Check playback state every tick (500ms)
                let playing = check_media_playing();
                if playing != IS_PLAYING {
                    IS_PLAYING = playing;
                    if APP_SETTINGS.show_play {
                        update_play_icon_only();
                    }
                }

                // Check theme every ~4th tick (~2s)
                THEME_CHECK_COUNTER += 1;
                if THEME_CHECK_COUNTER >= 4 {
                    THEME_CHECK_COUNTER = 0;
                    let system_dark = !is_system_light_theme();
                    if system_dark != LAST_THEME_DARK {
                        LAST_THEME_DARK = system_dark;
                        APP_SETTINGS.dark_icons = !system_dark;
                        update_tray_icons();
                        save_settings();
                    }
                }
            }
            LRESULT(0)
        }
        WM_TRAYICON => {
            let icon_id = wparam.0 as u32;
            let mouse_msg = (lparam.0 & 0xFFFF) as u32;

            match mouse_msg {
                WM_LBUTTONUP => handle_left_click(icon_id),
                WM_RBUTTONUP => show_context_menu(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let menu_id = (wparam.0 & 0xFFFF) as u16;
            handle_menu_command(menu_id);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn handle_left_click(icon_id: u32) {
    match icon_id {
        ICON_ID_DEFAULT => show_context_menu(MAIN_HWND),
        ICON_ID_PREV => send_media_key(VK_MEDIA_PREV_TRACK),
        ICON_ID_PLAY => {
            send_media_key(VK_MEDIA_PLAY_PAUSE);
            IS_PLAYING = !IS_PLAYING;
            update_play_icon_only();
        }
        ICON_ID_NEXT => send_media_key(VK_MEDIA_NEXT_TRACK),
        _ => {}
    }
}

unsafe fn send_media_key(vk: VIRTUAL_KEY) {
    let mut inputs: [INPUT; 2] = zeroed();

    inputs[0].r#type = INPUT_KEYBOARD;
    inputs[0].Anonymous.ki = KEYBDINPUT {
        wVk: vk,
        wScan: 0,
        dwFlags: KEYBD_EVENT_FLAGS(0),
        time: 0,
        dwExtraInfo: 0,
    };

    inputs[1].r#type = INPUT_KEYBOARD;
    inputs[1].Anonymous.ki = KEYBDINPUT {
        wVk: vk,
        wScan: 0,
        dwFlags: KEYEVENTF_KEYUP,
        time: 0,
        dwExtraInfo: 0,
    };

    let _ = SendInput(&inputs, size_of::<INPUT>() as i32);
}

unsafe fn show_context_menu(hwnd: HWND) {
    let hmenu = CreatePopupMenu().unwrap();

    let mut flags_prev = MF_STRING;
    let mut flags_play = MF_STRING;
    let mut flags_next = MF_STRING;

    if APP_SETTINGS.show_prev { flags_prev |= MF_CHECKED; }
    if APP_SETTINGS.show_play { flags_play |= MF_CHECKED; }
    if APP_SETTINGS.show_next { flags_next |= MF_CHECKED; }

    let _ = AppendMenuW(hmenu, flags_prev, MENU_SHOW_PREV as usize, w!("Show Previous"));
    let _ = AppendMenuW(hmenu, flags_play, MENU_SHOW_PLAY as usize, w!("Show Play/Pause"));
    let _ = AppendMenuW(hmenu, flags_next, MENU_SHOW_NEXT as usize, w!("Show Next"));
    let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, None);
    let _ = AppendMenuW(hmenu, MF_STRING, MENU_EXIT as usize, w!("Exit"));

    let mut pt: POINT = zeroed();
    let _ = GetCursorPos(&mut pt);

    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenu(hmenu, TPM_RIGHTALIGN | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, None);
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));

    let _ = DestroyMenu(hmenu);
}

unsafe fn handle_menu_command(menu_id: u16) {
    match menu_id {
        MENU_SHOW_PREV => {
            APP_SETTINGS.show_prev = !APP_SETTINGS.show_prev;
            update_tray_icons();
            save_settings();
        }
        MENU_SHOW_PLAY => {
            APP_SETTINGS.show_play = !APP_SETTINGS.show_play;
            update_tray_icons();
            save_settings();
        }
        MENU_SHOW_NEXT => {
            APP_SETTINGS.show_next = !APP_SETTINGS.show_next;
            update_tray_icons();
            save_settings();
        }
        MENU_EXIT => {
            let _ = PostMessageW(MAIN_HWND, WM_DESTROY, WPARAM(0), LPARAM(0));
        }
        _ => {}
    }
}

unsafe fn update_tray_icons() {
    remove_all_icons();

    let any_selected = APP_SETTINGS.show_prev || APP_SETTINGS.show_play || APP_SETTINGS.show_next;

    if !any_selected {
        add_tray_icon(ICON_ID_DEFAULT, create_default_icon(), "ClickPlay");
    } else {
        if APP_SETTINGS.show_prev {
            add_tray_icon(ICON_ID_PREV, create_prev_icon(), "Previous");
        }
        if APP_SETTINGS.show_play {
            let (icon, tip) = if IS_PLAYING {
                (create_pause_icon(), "Pause")
            } else {
                (create_play_icon(), "Play")
            };
            add_tray_icon(ICON_ID_PLAY, icon, tip);
        }
        if APP_SETTINGS.show_next {
            add_tray_icon(ICON_ID_NEXT, create_next_icon(), "Next");
        }
    }
}

unsafe fn update_play_icon_only() {
    if !APP_SETTINGS.show_play {
        return;
    }
    
    let (icon, tip) = if IS_PLAYING {
        (create_pause_icon(), "Pause")
    } else {
        (create_play_icon(), "Play")
    };
    
    let mut nid: NOTIFYICONDATAW = zeroed();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = MAIN_HWND;
    nid.uID = ICON_ID_PLAY;
    nid.uFlags = NIF_ICON | NIF_TIP;
    nid.hIcon = icon;
    
    let tip_wide: Vec<u16> = tip.encode_utf16().chain(std::iter::once(0)).collect();
    let len = tip_wide.len().min(128);
    nid.szTip[..len].copy_from_slice(&tip_wide[..len]);
    
    let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
}

unsafe fn add_tray_icon(id: u32, icon: HICON, tip: &str) {
    let mut nid: NOTIFYICONDATAW = zeroed();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = MAIN_HWND;
    nid.uID = id;
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAYICON;
    nid.hIcon = icon;

    let tip_wide: Vec<u16> = tip.encode_utf16().chain(std::iter::once(0)).collect();
    let len = tip_wide.len().min(128);
    nid.szTip[..len].copy_from_slice(&tip_wide[..len]);

    let _ = Shell_NotifyIconW(NIM_ADD, &nid);
}

unsafe fn remove_tray_icon(id: u32) {
    let mut nid: NOTIFYICONDATAW = zeroed();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = MAIN_HWND;
    nid.uID = id;
    let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
}

unsafe fn remove_all_icons() {
    remove_tray_icon(ICON_ID_DEFAULT);
    remove_tray_icon(ICON_ID_PREV);
    remove_tray_icon(ICON_ID_PLAY);
    remove_tray_icon(ICON_ID_NEXT);
}

// Icon size - 32x32
const ICON_SIZE: i32 = 32;
const ICON_PIXELS: usize = (ICON_SIZE * ICON_SIZE) as usize;

// ============== Icon Color ==============

fn get_icon_colors() -> (u8, u8, u8) {
    unsafe {
        if APP_SETTINGS.dark_icons {
            (0, 0, 0)
        } else {
            (255, 255, 255)
        }
    }
}

fn blend_color(r: u8, g: u8, b: u8, alpha: f32) -> u32 {
    let a = (alpha * 255.0).clamp(0.0, 255.0) as u8;
    ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

// ============== Icon Creation ==============

unsafe fn create_icon_from_pixels(pixels: &[u32]) -> HICON {
    let hdc = CreateCompatibleDC(None);
    
    let mut bmi: BITMAPINFO = zeroed();
    bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = ICON_SIZE;
    bmi.bmiHeader.biHeight = -ICON_SIZE;
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB.0;

    let mut bits: *mut std::ffi::c_void = null_mut();
    let hbmp = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
    
    if !bits.is_null() {
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u32, ICON_PIXELS);
    }

    let mut mask_bits: *mut std::ffi::c_void = null_mut();
    let hbmp_mask = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut mask_bits, None, 0).unwrap();
    if !mask_bits.is_null() {
        let mask_pixels: Vec<u32> = pixels.iter().map(|&p| {
            if (p >> 24) > 127 { 0x00000000 } else { 0xFFFFFFFF }
        }).collect();
        std::ptr::copy_nonoverlapping(mask_pixels.as_ptr(), mask_bits as *mut u32, ICON_PIXELS);
    }

    let icon_info = ICONINFO {
        fIcon: true.into(),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: hbmp_mask,
        hbmColor: hbmp,
    };

    let icon = CreateIconIndirect(&icon_info).unwrap_or(HICON(null_mut()));

    let _ = DeleteObject(hbmp);
    let _ = DeleteObject(hbmp_mask);
    let _ = DeleteDC(hdc);

    icon
}

fn set_pixel(pixels: &mut Vec<u32>, x: i32, y: i32, color: u32) {
    if x >= 0 && x < ICON_SIZE && y >= 0 && y < ICON_SIZE {
        pixels[(y * ICON_SIZE + x) as usize] = color;
    }
}

fn draw_filled_circle(pixels: &mut Vec<u32>, cx: f32, cy: f32, radius: f32, r: u8, g: u8, b: u8) {
    for py in 0..ICON_SIZE {
        for px in 0..ICON_SIZE {
            let dx = px as f32 + 0.5 - cx;
            let dy = py as f32 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            
            if dist <= radius - 0.7 {
                set_pixel(pixels, px, py, blend_color(r, g, b, 1.0));
            } else if dist <= radius + 0.7 {
                let alpha = 1.0 - (dist - radius + 0.7) / 1.4;
                let existing = pixels[(py * ICON_SIZE + px) as usize];
                let existing_alpha = (existing >> 24) as f32 / 255.0;
                let new_alpha = alpha.max(existing_alpha);
                set_pixel(pixels, px, py, blend_color(r, g, b, new_alpha));
            }
        }
    }
}

fn draw_rounded_rect(pixels: &mut Vec<u32>, x1: f32, y1: f32, x2: f32, y2: f32, corner_radius: f32, r: u8, g: u8, b: u8) {
    let cr = corner_radius.min((x2 - x1) / 2.0).min((y2 - y1) / 2.0);
    
    for py in 0..ICON_SIZE {
        for px in 0..ICON_SIZE {
            let fx = px as f32 + 0.5;
            let fy = py as f32 + 0.5;
            
            if fx < x1 - 0.7 || fx > x2 + 0.7 || fy < y1 - 0.7 || fy > y2 + 0.7 {
                continue;
            }
            
            let in_left = fx < x1 + cr;
            let in_right = fx > x2 - cr;
            let in_top = fy < y1 + cr;
            let in_bottom = fy > y2 - cr;
            
            let dist_to_edge = if in_left && in_top {
                let ccx = x1 + cr;
                let ccy = y1 + cr;
                cr - ((fx - ccx).powi(2) + (fy - ccy).powi(2)).sqrt()
            } else if in_right && in_top {
                let ccx = x2 - cr;
                let ccy = y1 + cr;
                cr - ((fx - ccx).powi(2) + (fy - ccy).powi(2)).sqrt()
            } else if in_left && in_bottom {
                let ccx = x1 + cr;
                let ccy = y2 - cr;
                cr - ((fx - ccx).powi(2) + (fy - ccy).powi(2)).sqrt()
            } else if in_right && in_bottom {
                let ccx = x2 - cr;
                let ccy = y2 - cr;
                cr - ((fx - ccx).powi(2) + (fy - ccy).powi(2)).sqrt()
            } else {
                (fx - x1).min(x2 - fx).min(fy - y1).min(y2 - fy)
            };
            
            if dist_to_edge > 0.7 {
                set_pixel(pixels, px, py, blend_color(r, g, b, 1.0));
            } else if dist_to_edge > -0.7 {
                let alpha = (dist_to_edge + 0.7) / 1.4;
                let existing = pixels[(py * ICON_SIZE + px) as usize];
                let existing_alpha = (existing >> 24) as f32 / 255.0;
                let new_alpha = alpha.max(existing_alpha);
                set_pixel(pixels, px, py, blend_color(r, g, b, new_alpha));
            }
        }
    }
}

fn draw_triangle_right(pixels: &mut Vec<u32>, x1: f32, y_center: f32, width: f32, height: f32, r: u8, g: u8, b: u8) {
    let half_height = height / 2.0;
    let x2 = x1 + width;
    
    for py in 0..ICON_SIZE {
        for px in 0..ICON_SIZE {
            let fx = px as f32 + 0.5;
            let fy = py as f32 + 0.5;
            
            let dy = (fy - y_center).abs();
            if dy > half_height + 1.0 {
                continue;
            }
            
            let progress = dy / half_height;
            let edge_x = x2 - progress * width;
            
            if fx >= x1 - 0.7 && fx <= edge_x + 0.7 {
                let left_dist = fx - x1;
                let right_dist = edge_x - fx;
                let tb_dist = half_height - dy;
                let min_dist = left_dist.min(right_dist).min(tb_dist);
                
                if min_dist > 0.7 {
                    set_pixel(pixels, px, py, blend_color(r, g, b, 1.0));
                } else if min_dist > -0.7 {
                    let alpha = (min_dist + 0.7) / 1.4;
                    let existing = pixels[(py * ICON_SIZE + px) as usize];
                    let existing_alpha = (existing >> 24) as f32 / 255.0;
                    let new_alpha = alpha.max(existing_alpha);
                    set_pixel(pixels, px, py, blend_color(r, g, b, new_alpha));
                }
            }
        }
    }
}

fn draw_triangle_left(pixels: &mut Vec<u32>, x_right: f32, y_center: f32, width: f32, height: f32, r: u8, g: u8, b: u8) {
    let half_height = height / 2.0;
    let x_left = x_right - width;
    
    for py in 0..ICON_SIZE {
        for px in 0..ICON_SIZE {
            let fx = px as f32 + 0.5;
            let fy = py as f32 + 0.5;
            
            let dy = (fy - y_center).abs();
            if dy > half_height + 1.0 {
                continue;
            }
            
            let progress = dy / half_height;
            let edge_x = x_left + progress * width;
            
            if fx <= x_right + 0.7 && fx >= edge_x - 0.7 {
                let left_dist = fx - edge_x;
                let right_dist = x_right - fx;
                let tb_dist = half_height - dy;
                let min_dist = left_dist.min(right_dist).min(tb_dist);
                
                if min_dist > 0.7 {
                    set_pixel(pixels, px, py, blend_color(r, g, b, 1.0));
                } else if min_dist > -0.7 {
                    let alpha = (min_dist + 0.7) / 1.4;
                    let existing = pixels[(py * ICON_SIZE + px) as usize];
                    let existing_alpha = (existing >> 24) as f32 / 255.0;
                    let new_alpha = alpha.max(existing_alpha);
                    set_pixel(pixels, px, py, blend_color(r, g, b, new_alpha));
                }
            }
        }
    }
}

// ============== Icon Definitions (32x32) ==============

unsafe fn create_default_icon() -> HICON {
    let mut pixels = vec![0x00000000u32; ICON_PIXELS];
    let (r, g, b) = get_icon_colors();
    
    // Music note head
    draw_filled_circle(&mut pixels, 10.0, 23.0, 7.0, r, g, b);
    
    // Note stem
    draw_rounded_rect(&mut pixels, 13.0, 5.0, 17.0, 23.0, 2.0, r, g, b);
    
    // Note flag
    draw_rounded_rect(&mut pixels, 14.0, 5.0, 25.0, 8.0, 2.0, r, g, b);
    draw_rounded_rect(&mut pixels, 22.0, 7.5, 24.5, 13.0, 1.5, r, g, b);
    draw_rounded_rect(&mut pixels, 21.0, 12.0, 23.0, 16.0, 1.0, r, g, b);

    create_icon_from_pixels(&pixels)
}

unsafe fn create_prev_icon() -> HICON {
    let mut pixels = vec![0x00000000u32; ICON_PIXELS];
    let (r, g, b) = get_icon_colors();
    
    // Left bar (y: 6-26 = height 20)
    draw_rounded_rect(&mut pixels, 5.0, 6.0, 10.0, 26.0, 1.5, r, g, b);
    
    // Triangle pointing left
    draw_triangle_left(&mut pixels, 27.0, 16.0, 19.0, 20.0, r, g, b);

    create_icon_from_pixels(&pixels)
}

unsafe fn create_play_icon() -> HICON {
    let mut pixels = vec![0x00000000u32; ICON_PIXELS];
    let (r, g, b) = get_icon_colors();
    
    // Play triangle
    draw_triangle_right(&mut pixels, 7.0, 16.0, 21.0, 20.0, r, g, b);

    create_icon_from_pixels(&pixels)
}

unsafe fn create_pause_icon() -> HICON {
    let mut pixels = vec![0x00000000u32; ICON_PIXELS];
    let (r, g, b) = get_icon_colors();
    
    // Two vertical bars
    draw_rounded_rect(&mut pixels, 6.0, 6.0, 12.0, 26.0, 1.5, r, g, b);
    draw_rounded_rect(&mut pixels, 20.0, 6.0, 26.0, 26.0, 1.5, r, g, b);

    create_icon_from_pixels(&pixels)
}

unsafe fn create_next_icon() -> HICON {
    let mut pixels = vec![0x00000000u32; ICON_PIXELS];
    let (r, g, b) = get_icon_colors();
    
    // Triangle pointing right
    draw_triangle_right(&mut pixels, 5.0, 16.0, 19.0, 20.0, r, g, b);
    
    // Right bar (y: 6-26 = height 20)
    draw_rounded_rect(&mut pixels, 22.0, 6.0, 27.0, 26.0, 1.5, r, g, b);

    create_icon_from_pixels(&pixels)
}

// ============== Media State Detection ==============

fn init_media_manager() -> Option<GlobalSystemMediaTransportControlsSessionManager> {
    GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
        .ok()
        .and_then(|op| op.get().ok())
}

fn check_media_playing() -> bool {
    unsafe {
        if let Some(ref manager) = MEDIA_MANAGER {
            if let Ok(session) = manager.GetCurrentSession() {
                if let Ok(info) = session.GetPlaybackInfo() {
                    if let Ok(status) = info.PlaybackStatus() {
                        return status == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing;
                    }
                }
            }
        }
    }
    false
}

// ============== Color Theme Detection ==============

fn is_system_light_theme() -> bool {
    unsafe {
        let mut hkey: HKEY = HKEY(null_mut());
        let subkey = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
        
        if RegOpenKeyExW(HKEY_CURRENT_USER, subkey, 0, KEY_READ, &mut hkey).is_ok() {
            let mut value: u32 = 0;
            let mut size: u32 = 4;
            let mut reg_type = REG_VALUE_TYPE::default();
            
            if RegQueryValueExW(
                hkey,
                w!("SystemUsesLightTheme"),
                None,
                Some(&mut reg_type),
                Some(&mut value as *mut u32 as *mut u8),
                Some(&mut size),
            ).is_ok() {
                let _ = windows::Win32::System::Registry::RegCloseKey(hkey);
                return value == 1;
            }
            
            let _ = windows::Win32::System::Registry::RegCloseKey(hkey);
        }
        
        false
    }
}

// ============== Config File Storage ==============

fn get_config_path() -> PathBuf {
    let exe_path = env::current_exe().unwrap_or_default();
    let exe_dir = exe_path.parent().unwrap_or(std::path::Path::new("."));
    exe_dir.join("clickplay.cfg")
}

fn save_settings() {
    unsafe {
        let config = format!(
            "prev={}\nplay={}\nnext={}\ndark_icons={}",
            APP_SETTINGS.show_prev as u8,
            APP_SETTINGS.show_play as u8,
            APP_SETTINGS.show_next as u8,
            APP_SETTINGS.dark_icons as u8
        );
        let _ = fs::write(get_config_path(), config);
    }
}

fn load_settings() -> AppSettings {
    let mut settings = AppSettings::default();
    
    if let Ok(content) = fs::read_to_string(get_config_path()) {
        for line in content.lines() {
            let parts: Vec<&str> = line.split('=').collect();
            if parts.len() == 2 {
                let value = parts[1].trim() == "1";
                match parts[0].trim() {
                    "prev" => settings.show_prev = value,
                    "play" => settings.show_play = value,
                    "next" => settings.show_next = value,
                    "dark_icons" => settings.dark_icons = value,
                    _ => {}
                }
            }
        }
    }
    
    settings
}
