//! Playback viewport sizing, independent of the incoming video resolution.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowSize {
    Small,
    #[default]
    Medium,
    Large,
    Fit,
}

impl WindowSize {
    pub fn dimensions(self, video: (usize, usize), available: (usize, usize)) -> (usize, usize) {
        let edge = match self {
            Self::Small => 640,
            Self::Medium => 960,
            Self::Large => 1280,
            Self::Fit => usize::MAX,
        };
        let width = video.0.max(1) as u64;
        let height = video.1.max(1) as u64;
        let max_width = available.0.min(edge).max(1) as u64;
        let max_height = available.1.min(edge).max(1) as u64;
        if max_width * height <= max_height * width {
            (
                max_width as usize,
                (height * max_width / width).max(1) as usize,
            )
        } else {
            (
                (width * max_height / height).max(1) as usize,
                max_height as usize,
            )
        }
    }
}

/// Resize the existing HWND, preserving the stream, decoder, menu, and focus.
#[cfg(windows)]
pub fn apply(window: &minifb::Window, size: WindowSize, video: (usize, usize)) {
    use std::mem::{size_of, zeroed};
    use std::ptr::null_mut;
    use winapi::shared::windef::{HWND, RECT};
    use winapi::um::wingdi::{SetBrushOrgEx, SetStretchBltMode, HALFTONE};
    use winapi::um::winuser::*;

    // minifb owns this live HWND, and all calls occur on its render thread.
    unsafe {
        let hwnd = window.get_window_handle() as HWND;
        // minifb uses StretchDIBits on a CS_OWNDC window without choosing a
        // stretch mode. Windows defaults to BLACKONWHITE, whose bitwise AND
        // corrupts colors when shrinking. This DC persists through repaints,
        // manual resizing and phone rotation. HALFTONE averages color pixels.
        let dc = GetDC(hwnd);
        if !dc.is_null() {
            if SetStretchBltMode(dc, HALFTONE) == 0 || SetBrushOrgEx(dc, 0, 0, null_mut()) == 0 {
                tracing::warn!("Could not configure color-preserving playback scaling");
            }
            ReleaseDC(hwnd, dc);
        }
        if IsZoomed(hwnd) != 0 || IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            tracing::warn!("Could not determine playback monitor work area");
            return;
        }
        let work = info.rcWork;
        let mut border: RECT = zeroed();
        if AdjustWindowRectEx(
            &mut border,
            GetWindowLongW(hwnd, GWL_STYLE) as u32,
            i32::from(!GetMenu(hwnd).is_null()),
            GetWindowLongW(hwnd, GWL_EXSTYLE) as u32,
        ) == 0
        {
            return;
        }
        let border_width = border.right - border.left;
        let border_height = border.bottom - border.top;
        let available = (
            (work.right - work.left - border_width).max(1) as usize,
            (work.bottom - work.top - border_height).max(1) as usize,
        );
        let (width, height) = size.dimensions(video, available);
        let outer_width = width as i32 + border_width;
        let outer_height = height as i32 + border_height;
        let mut current: RECT = zeroed();
        if GetWindowRect(hwnd, &mut current) == 0 {
            return;
        }
        let x = current
            .left
            .clamp(work.left, (work.right - outer_width).max(work.left));
        let y = current
            .top
            .clamp(work.top, (work.bottom - outer_height).max(work.top));
        if SetWindowPos(
            hwnd,
            null_mut(),
            x,
            y,
            outer_width,
            outer_height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        ) == 0
        {
            tracing::warn!("Could not resize playback window");
        }
    }
}

#[cfg(not(windows))]
pub fn apply(_window: &minifb::Window, _size: WindowSize, _video: (usize, usize)) {}

#[cfg(windows)]
pub fn close_for_shutdown(handle: isize) {
    use winapi::shared::windef::HWND;
    use winapi::um::winuser::{PostMessageW, WM_CANCELMODE, WM_CLOSE};
    if handle != 0 {
        // Exit a native menu/resize modal loop before joining the render thread.
        unsafe {
            PostMessageW(handle as HWND, WM_CANCELMODE, 0, 0);
            PostMessageW(handle as HWND, WM_CLOSE, 0, 0);
        }
    }
}

#[cfg(not(windows))]
pub fn close_for_shutdown(_handle: isize) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_landscape_and_portrait_without_changing_aspect_ratio() {
        assert_eq!(
            WindowSize::Medium.dimensions((1920, 1080), (1900, 1000)),
            (960, 540)
        );
        assert_eq!(
            WindowSize::Medium.dimensions((1080, 1920), (1900, 1000)),
            (540, 960)
        );
        assert_eq!(
            WindowSize::Large.dimensions((1080, 1920), (1200, 900)),
            (506, 900)
        );
        assert_eq!(
            WindowSize::Fit.dimensions((1920, 1080), (1200, 900)),
            (1200, 675)
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "creates a native window to verify its actual GDI scaling context"]
    fn native_downscaling_preserves_colors() {
        use std::mem::{size_of, zeroed};
        use std::ptr::null_mut;
        use winapi::um::{wingdi::*, winuser::*};

        let window = minifb::Window::new(
            "UxPlayRs scaling regression",
            320,
            192,
            minifb::WindowOptions::default(),
        )
        .unwrap();
        apply(&window, WindowSize::Medium, (1920, 1080));
        // Exercise the mode of the real renderer's CS_OWNDC, then reproduce
        // its downscale into an offscreen DIB for a deterministic pixel check.
        unsafe {
            let hwnd = window.get_window_handle() as winapi::shared::windef::HWND;
            ShowWindow(hwnd, SW_HIDE);
            let display_dc = GetDC(hwnd);
            assert!(!display_dc.is_null());
            let mode = GetStretchBltMode(display_dc);
            let target_dc = CreateCompatibleDC(display_dc);
            ReleaseDC(hwnd, display_dc);
            assert!(!target_dc.is_null());
            SetStretchBltMode(target_dc, mode);
            SetBrushOrgEx(target_dc, 0, 0, null_mut());
            let mut target_info: BITMAPINFO = zeroed();
            target_info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            target_info.bmiHeader.biWidth = 1;
            target_info.bmiHeader.biHeight = -1;
            target_info.bmiHeader.biPlanes = 1;
            target_info.bmiHeader.biBitCount = 32;
            target_info.bmiHeader.biCompression = BI_RGB;
            let mut target_bits = null_mut();
            let bitmap = CreateDIBSection(
                target_dc,
                &target_info,
                DIB_RGB_COLORS,
                &mut target_bits,
                null_mut(),
                0,
            );
            assert!(!bitmap.is_null());
            let previous = SelectObject(target_dc, bitmap as _);
            let mut source_info = target_info;
            source_info.bmiHeader.biWidth = 2;
            source_info.bmiHeader.biHeight = -2;
            let source = [0x007f7f7fu32, 0x00808080, 0x00808080, 0x007f7f7f];
            let rows = StretchDIBits(
                target_dc,
                0,
                0,
                1,
                1,
                0,
                0,
                2,
                2,
                source.as_ptr() as _,
                &source_info,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
            GdiFlush();
            let pixel = *(target_bits as *const u32) & 0x00ffffff;
            SelectObject(target_dc, previous);
            DeleteObject(bitmap as _);
            DeleteDC(target_dc);
            assert_ne!(rows, 0);
            eprintln!("GDI stretch mode {mode}, averaged gray pixel {pixel:#08x}");
            // Gray 127 and 128 must average near 128, not turn black (127 & 128).
            for shift in [0, 8, 16] {
                let channel = (pixel >> shift) & 255;
                assert!(
                    (124..=132).contains(&channel),
                    "downscale changed gray to {pixel:#08x}"
                );
            }
        }
    }
}
