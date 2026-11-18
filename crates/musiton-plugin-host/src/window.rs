//! A native top-level window that holds a plugin's own editor.
//!
//! Only Windows is supported: the window is a plain Win32 window on the UI thread, whose
//! messages are dispatched by the app's event loop like those of its main window. On
//! other systems no editor windows are opened (plugins show generic controls instead).

use std::ffi::c_void;

/// Whether this system can show plugin editors.
pub(crate) const SUPPORTED: bool = cfg!(windows);

#[cfg(windows)]
mod imp {
    use std::cell::Cell;
    use std::ffi::c_void;

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW,
        LoadCursorW, RegisterClassExW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos,
        ShowWindow, CW_USEDEFAULT, GWLP_USERDATA, IDC_ARROW, SWP_NOMOVE, SWP_NOZORDER, SW_SHOW,
        WINDOW_STYLE, WM_CLOSE, WM_SIZE, WNDCLASSEXW, WS_CAPTION, WS_MINIMIZEBOX, WS_OVERLAPPED,
        WS_SYSMENU, WS_THICKFRAME,
    };

    /// What the window procedure reports back to the owner.
    struct State {
        closed: Cell<bool>,
        resized: Cell<Option<(i32, i32)>>,
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    const CLASS: &str = "MusitonPluginEditor";

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        // SAFETY: the user data is our `State`, set right after creation and valid until
        // the window is destroyed (the owner destroys the window before freeing it).
        let state = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const State).as_ref() };
        match (msg, state) {
            (WM_CLOSE, Some(s)) => {
                // The owner detaches the plugin first, then destroys the window.
                s.closed.set(true);
                0
            }
            (WM_SIZE, Some(s)) => {
                let w = (lp & 0xFFFF) as i32;
                let h = ((lp >> 16) & 0xFFFF) as i32;
                if w > 0 && h > 0 {
                    s.resized.set(Some((w, h)));
                }
                // SAFETY: default handling of a message for our own window.
                unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
            }
            // SAFETY: as above.
            _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
        }
    }

    fn style(resizable: bool) -> WINDOW_STYLE {
        let base = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        if resizable {
            base | WS_THICKFRAME
        } else {
            base
        }
    }

    /// An editor window.
    pub(crate) struct Window {
        hwnd: HWND,
        state: Box<State>,
        resizable: bool,
    }

    impl Window {
        pub(crate) fn new(title: &str, width: i32, height: i32, resizable: bool) -> Option<Self> {
            let class = wide(CLASS);
            // SAFETY: plain Win32 calls with valid, NUL-terminated strings; registering
            // the class twice fails harmlessly.
            unsafe {
                let instance = GetModuleHandleW(std::ptr::null());
                let wc = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    style: 0,
                    lpfnWndProc: Some(wndproc),
                    cbClsExtra: 0,
                    cbWndExtra: 0,
                    hInstance: instance,
                    hIcon: std::ptr::null_mut(),
                    hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                    hbrBackground: std::ptr::null_mut(),
                    lpszMenuName: std::ptr::null(),
                    lpszClassName: class.as_ptr(),
                    hIconSm: std::ptr::null_mut(),
                };
                RegisterClassExW(&wc);
                let (w, h) = outer_size(width, height, resizable);
                let hwnd = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    wide(title).as_ptr(),
                    style(resizable),
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    w,
                    h,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                );
                if hwnd.is_null() {
                    return None;
                }
                let state = Box::new(State {
                    closed: Cell::new(false),
                    resized: Cell::new(None),
                });
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, &*state as *const State as isize);
                Some(Self {
                    hwnd,
                    state,
                    resizable,
                })
            }
        }

        pub(crate) fn handle(&self) -> *mut c_void {
            self.hwnd
        }

        pub(crate) fn show(&self) {
            // SAFETY: our own window.
            unsafe {
                ShowWindow(self.hwnd, SW_SHOW);
                SetForegroundWindow(self.hwnd);
            }
        }

        pub(crate) fn set_client_size(&self, width: i32, height: i32) {
            set_client_size(self.hwnd, width, height, self.resizable);
        }

        pub(crate) fn closed(&self) -> bool {
            self.state.closed.get()
        }

        pub(crate) fn take_resized(&self) -> Option<(i32, i32)> {
            self.state.resized.take()
        }
    }

    impl Drop for Window {
        fn drop(&mut self) {
            // SAFETY: our own window; the window procedure stops using the state once the
            // user data is cleared, before the state is freed.
            unsafe {
                SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
                DestroyWindow(self.hwnd);
            }
        }
    }

    fn outer_size(width: i32, height: i32, resizable: bool) -> (i32, i32) {
        let mut r = RECT {
            left: 0,
            top: 0,
            right: width.max(1),
            bottom: height.max(1),
        };
        // SAFETY: valid RECT pointer.
        unsafe { AdjustWindowRectEx(&mut r, style(resizable), 0, 0) };
        (r.right - r.left, r.bottom - r.top)
    }

    /// Resizes the window `hwnd` so its inside is `width` × `height`.
    fn set_client_size(hwnd: *mut c_void, width: i32, height: i32, resizable: bool) {
        let (w, h) = outer_size(width, height, resizable);
        // SAFETY: plain call; an invalid handle makes it fail without effect.
        unsafe {
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                w,
                h,
                SWP_NOMOVE | SWP_NOZORDER,
            )
        };
    }
}

#[cfg(not(windows))]
mod imp {
    use std::ffi::c_void;

    /// Editor windows are not supported on this system.
    pub(crate) struct Window;

    impl Window {
        pub(crate) fn new(
            _title: &str,
            _width: i32,
            _height: i32,
            _resizable: bool,
        ) -> Option<Self> {
            None
        }
        pub(crate) fn handle(&self) -> *mut c_void {
            std::ptr::null_mut()
        }
        pub(crate) fn show(&self) {}
        pub(crate) fn set_client_size(&self, _width: i32, _height: i32) {}
        pub(crate) fn closed(&self) -> bool {
            true
        }
        pub(crate) fn take_resized(&self) -> Option<(i32, i32)> {
            None
        }
    }
}

pub(crate) use imp::Window;

/// A raw window handle (for the plugin APIs).
pub(crate) type Handle = *mut c_void;
