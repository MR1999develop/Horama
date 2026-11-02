//! Windows "Open With" (Apri con) integration using native Shell32 SHAssocEnumHandlers API
//! and native icon extraction for registered Windows applications.

#[derive(Debug, Clone)]
pub struct OpenWithItem {
    pub id: i32,
    pub name: String,
    pub icon_rgba: Option<Vec<u8>>,
    pub icon_width: u32,
    pub icon_height: u32,
}

#[cfg(target_os = "windows")]
mod win_impl {
    // Win32 structures keep their SDK names so they can be checked against the documentation.
    #![allow(clippy::upper_case_acronyms)]
    use super::OpenWithItem;
    use std::ffi::c_void;
    use std::path::Path;
    use std::ptr;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct GUID {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    const IID_ISHELLITEM: GUID = GUID {
        data1: 0x43826d1e,
        data2: 0xe718,
        data3: 0x42ee,
        data4: [0xbc, 0x55, 0xa1, 0xe2, 0x61, 0xc3, 0x7b, 0xfe],
    };

    const IID_IDATAOBJECT: GUID = GUID {
        data1: 0x0000010e,
        data2: 0x0000,
        data3: 0x0000,
        data4: [0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
    };

    const BHID_DATAOBJECT: GUID = GUID {
        data1: 0xb8c0bd9f,
        data2: 0xed24,
        data3: 0x455c,
        data4: [0x83, 0xe6, 0xd5, 0x39, 0x0c, 0x4f, 0xe8, 0xc4],
    };

    #[repr(C)]
    struct ICONINFO {
        f_icon: i32,
        x_hotspot: u32,
        y_hotspot: u32,
        hbm_mask: *mut c_void,
        hbm_color: *mut c_void,
    }

    #[repr(C)]
    struct BITMAPINFOHEADER {
        bi_size: u32,
        bi_width: i32,
        bi_height: i32,
        bi_planes: u16,
        bi_bit_count: u16,
        bi_compression: u32,
        bi_size_image: u32,
        bi_x_pels_per_meter: i32,
        bi_y_pels_per_meter: i32,
        bi_clr_used: u32,
        bi_clr_important: u32,
    }

    #[repr(C)]
    struct SHFILEINFOW {
        h_icon: *mut c_void,
        i_icon: i32,
        dw_attributes: u32,
        sz_display_name: [u16; 260],
        sz_type_name: [u16; 80],
    }

    #[link(name = "shell32")]
    #[link(name = "ole32")]
    #[link(name = "user32")]
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CoInitializeEx(pvReserved: *mut c_void, dwCoInit: u32) -> i32;
        fn CoUninitialize();
        fn GetObjectW(h: *mut c_void, c: i32, pv: *mut c_void) -> i32;
        fn CoTaskMemFree(pv: *mut c_void);

        fn SHAssocEnumHandlers(
            pszExtra: *const u16,
            afFilter: i32,
            ppEnumHandler: *mut *mut c_void,
        ) -> i32;

        fn SHCreateItemFromParsingName(
            pszPath: *const u16,
            pbc: *mut c_void,
            riid: *const GUID,
            ppv: *mut *mut c_void,
        ) -> i32;

        fn SHGetFileInfoW(
            pszPath: *const u16,
            dwFileAttributes: u32,
            psfi: *mut SHFILEINFOW,
            cbFileInfo: u32,
            uFlags: u32,
        ) -> usize;

        fn ExtractIconExW(
            lpszFile: *const u16,
            nIconIndex: i32,
            phiconLarge: *mut *mut c_void,
            phiconSmall: *mut *mut c_void,
            nIcons: u32,
        ) -> u32;

        fn GetIconInfo(hIcon: *mut c_void, piconinfo: *mut ICONINFO) -> i32;
        fn DestroyIcon(hIcon: *mut c_void) -> i32;
        fn GetDC(hWnd: *mut c_void) -> *mut c_void;
        fn ReleaseDC(hWnd: *mut c_void, hDC: *mut c_void) -> i32;
        fn DeleteObject(ho: *mut c_void) -> i32;

        fn GetDIBits(
            hdc: *mut c_void,
            hbm: *mut c_void,
            start: u32,
            cLines: u32,
            lpvBits: *mut c_void,
            lpbmi: *mut BITMAPINFOHEADER,
            usage: u32,
        ) -> i32;
    }

    #[repr(C)]
    struct BITMAP {
        bm_type: i32,
        bm_width: i32,
        bm_height: i32,
        bm_width_bytes: i32,
        bm_planes: u16,
        bm_bits_pixel: u16,
        bm_bits: *mut c_void,
    }

    /// Balanced COM initialization for the current thread.
    struct ComGuard(bool);
    impl ComGuard {
        fn new() -> Self {
            // COINIT_APARTMENTTHREADED = 2. S_OK (0) and S_FALSE (1) both require CoUninitialize.
            let hr = unsafe { CoInitializeEx(ptr::null_mut(), 2) };
            ComGuard(hr >= 0)
        }
    }
    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    type VTable = *const *const usize;

    unsafe fn call_release(p: *mut c_void) { unsafe {
        if !p.is_null() {
            let vt = *(p as VTable);
            let release_fn: extern "system" fn(*mut c_void) -> u32 =
                std::mem::transmute(*vt.add(2));
            release_fn(p);
        }
    }}

    /// Extract 16x16 32-bit RGBA pixels from an HICON.
    unsafe fn hicon_to_rgba(h_icon: *mut c_void) -> Option<(u32, u32, Vec<u8>)> { unsafe {
        if h_icon.is_null() {
            return None;
        }

        let mut ii = std::mem::zeroed::<ICONINFO>();
        if GetIconInfo(h_icon, &mut ii) == 0 {
            return None;
        }

        let hdc = GetDC(ptr::null_mut());
        if hdc.is_null() {
            if !ii.hbm_color.is_null() {
                DeleteObject(ii.hbm_color);
            }
            if !ii.hbm_mask.is_null() {
                DeleteObject(ii.hbm_mask);
            }
            return None;
        }

        let target_bitmap = if !ii.hbm_color.is_null() {
            ii.hbm_color
        } else {
            ii.hbm_mask
        };
        let mut bm: BITMAP = std::mem::zeroed();
        let got = GetObjectW(
            target_bitmap,
            std::mem::size_of::<BITMAP>() as i32,
            &mut bm as *mut BITMAP as *mut c_void,
        );
        let width = if got != 0 && bm.bm_width > 0 {
            bm.bm_width.min(256)
        } else {
            16
        };
        let mut height = if got != 0 && bm.bm_height > 0 {
            bm.bm_height.min(256)
        } else {
            16
        };
        if ii.hbm_color.is_null() {
            // Monochrome icons store AND and XOR masks stacked vertically.
            height = (height / 2).max(1);
        }
        let mut bmi = std::mem::zeroed::<BITMAPINFOHEADER>();
        bmi.bi_size = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bi_width = width;
        bmi.bi_height = -height; // top-down
        bmi.bi_planes = 1;
        bmi.bi_bit_count = 32;
        bmi.bi_compression = 0; // BI_RGB

        let mut buf = vec![0u8; (width * height * 4) as usize];

        let lines = GetDIBits(
            hdc,
            target_bitmap,
            0,
            height as u32,
            buf.as_mut_ptr() as *mut c_void,
            &mut bmi,
            0,
        );

        ReleaseDC(ptr::null_mut(), hdc);
        if !ii.hbm_color.is_null() {
            DeleteObject(ii.hbm_color);
        }
        if !ii.hbm_mask.is_null() {
            DeleteObject(ii.hbm_mask);
        }

        if lines == 0 {
            return None;
        }

        // Convert BGRA to RGBA. Icons without an alpha channel (all alpha = 0) are opaque.
        let has_alpha = buf.as_chunks::<4>().0.iter().any(|c| c[3] != 0);
        for chunk in buf.as_chunks_mut::<4>().0 {
            chunk.swap(0, 2);
            if !has_alpha {
                chunk[3] = 255;
            }
        }

        Some((width as u32, height as u32, buf))
    }}

    /// Extract icon for a given file path or application name.
    unsafe fn extract_app_icon(app_path: &str, icon_index: i32) -> Option<(u32, u32, Vec<u8>)> { unsafe {
        let wide: Vec<u16> = app_path.encode_utf16().chain(std::iter::once(0)).collect();

        // 1. Try ExtractIconExW
        let mut h_small: *mut c_void = ptr::null_mut();
        if ExtractIconExW(wide.as_ptr(), icon_index, ptr::null_mut(), &mut h_small, 1) > 0
            && !h_small.is_null()
        {
            let res = hicon_to_rgba(h_small);
            DestroyIcon(h_small);
            if res.is_some() {
                return res;
            }
        }

        // 2. Try SHGetFileInfoW
        let mut sfi = std::mem::zeroed::<SHFILEINFOW>();
        let res = SHGetFileInfoW(
            wide.as_ptr(),
            0,
            &mut sfi,
            std::mem::size_of::<SHFILEINFOW>() as u32,
            0x101, // SHGFI_ICON | SHGFI_SMALLICON
        );

        if res != 0 && !sfi.h_icon.is_null() {
            let res_rgba = hicon_to_rgba(sfi.h_icon);
            DestroyIcon(sfi.h_icon);
            return res_rgba;
        }

        None
    }}

    pub fn get_handlers_for_file(file_path: &str) -> Vec<OpenWithItem> {
        let mut results = Vec::new();
        let p = Path::new(file_path);
        let ext = match p.extension().and_then(|e| e.to_str()) {
            Some(e) => format!(".{}", e),
            None => return results,
        };

        let ext_wide: Vec<u16> = ext.encode_utf16().chain(std::iter::once(0)).collect();

        let _com = ComGuard::new();
        unsafe {
            let mut enum_handlers: *mut c_void = ptr::null_mut();
            // ASSOC_FILTER_RECOMMENDED = 1
            let hr = SHAssocEnumHandlers(ext_wide.as_ptr(), 1, &mut enum_handlers);
            if hr == 0 && !enum_handlers.is_null() {
                let vt = *(enum_handlers as VTable);
                let next_fn: extern "system" fn(
                    *mut c_void,
                    u32,
                    *mut *mut c_void,
                    *mut u32,
                ) -> i32 = std::mem::transmute(*vt.add(3));

                // `enum_idx` counts every handler returned by the enumerator, so the id stored
                // in each item matches the position used by `launch_handler`.
                let mut enum_idx: i32 = -1;
                loop {
                    let mut handler: *mut c_void = ptr::null_mut();
                    let mut fetched = 0u32;
                    let r = next_fn(enum_handlers, 1, &mut handler, &mut fetched);
                    if r != 0 || fetched == 0 || handler.is_null() {
                        break;
                    }
                    enum_idx += 1;

                    let h_vt = *(handler as VTable);
                    let get_ui_name: extern "system" fn(*mut c_void, *mut *mut u16) -> i32 =
                        std::mem::transmute(*h_vt.add(4));
                    let get_name: extern "system" fn(*mut c_void, *mut *mut u16) -> i32 =
                        std::mem::transmute(*h_vt.add(3));
                    let get_icon_loc: extern "system" fn(
                        *mut c_void,
                        *mut *mut u16,
                        *mut i32,
                    ) -> i32 = std::mem::transmute(*h_vt.add(5));

                    let mut ui_name_ptr: *mut u16 = ptr::null_mut();
                    let mut name_ptr: *mut u16 = ptr::null_mut();
                    let mut icon_loc_ptr: *mut u16 = ptr::null_mut();
                    let mut icon_idx: i32 = 0;

                    let mut ui_name_str = String::new();
                    let mut path_for_icon = String::new();

                    if get_ui_name(handler, &mut ui_name_ptr) == 0 && !ui_name_ptr.is_null() {
                        let mut len = 0;
                        while *ui_name_ptr.add(len) != 0 {
                            len += 1;
                        }
                        ui_name_str =
                            String::from_utf16_lossy(std::slice::from_raw_parts(ui_name_ptr, len));
                        CoTaskMemFree(ui_name_ptr as *mut c_void);
                    }

                    if get_icon_loc(handler, &mut icon_loc_ptr, &mut icon_idx) == 0
                        && !icon_loc_ptr.is_null()
                    {
                        let mut len = 0;
                        while *icon_loc_ptr.add(len) != 0 {
                            len += 1;
                        }
                        let s =
                            String::from_utf16_lossy(std::slice::from_raw_parts(icon_loc_ptr, len));
                        CoTaskMemFree(icon_loc_ptr as *mut c_void);
                        if !s.starts_with('@') && !s.is_empty() {
                            path_for_icon = s;
                        }
                    }

                    if path_for_icon.is_empty()
                        && get_name(handler, &mut name_ptr) == 0
                        && !name_ptr.is_null()
                    {
                        let mut len = 0;
                        while *name_ptr.add(len) != 0 {
                            len += 1;
                        }
                        let s = String::from_utf16_lossy(std::slice::from_raw_parts(name_ptr, len));
                        CoTaskMemFree(name_ptr as *mut c_void);
                        if s.to_lowercase().ends_with(".exe") {
                            path_for_icon = s;
                            icon_idx = 0;
                        }
                    }

                    let trimmed = ui_name_str.trim().to_string();
                    if !trimmed.is_empty()
                        && trimmed != "None"
                        && !results
                            .iter()
                            .any(|item: &OpenWithItem| item.name == trimmed)
                    {
                        let icon_data = if !path_for_icon.is_empty() {
                            extract_app_icon(&path_for_icon, icon_idx)
                        } else {
                            None
                        };

                        let (icon_width, icon_height, icon_rgba) = match icon_data {
                            Some((w, h, rgba)) => (w, h, Some(rgba)),
                            None => (0, 0, None),
                        };

                        results.push(OpenWithItem {
                            id: enum_idx,
                            name: trimmed,
                            icon_rgba,
                            icon_width,
                            icon_height,
                        });
                    }

                    call_release(handler);
                }

                call_release(enum_handlers);
            }
        }

        results
    }

    pub fn launch_handler(file_path: &str, target_idx: i32) -> Result<(), String> {
        let p = Path::new(file_path);
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{}", e))
            .unwrap_or_default();
        if ext.is_empty() {
            return Err("File has no extension".to_string());
        }

        let ext_wide: Vec<u16> = ext.encode_utf16().chain(std::iter::once(0)).collect();
        let file_wide: Vec<u16> = file_path.encode_utf16().chain(std::iter::once(0)).collect();

        let _com = ComGuard::new();
        unsafe {
            // 1. Create ShellItem and DataObject for the file
            let mut shell_item: *mut c_void = ptr::null_mut();
            let hr_item = SHCreateItemFromParsingName(
                file_wide.as_ptr(),
                ptr::null_mut(),
                &IID_ISHELLITEM,
                &mut shell_item,
            );
            if hr_item != 0 || shell_item.is_null() {
                return Err(format!(
                    "Failed to create shell item for file: {:#x}",
                    hr_item
                ));
            }

            let item_vt = *(shell_item as VTable);
            let bind_to_handler: extern "system" fn(
                *mut c_void,
                *mut c_void,
                *const GUID,
                *const GUID,
                *mut *mut c_void,
            ) -> i32 = std::mem::transmute(*item_vt.add(3));

            let mut data_object: *mut c_void = ptr::null_mut();
            let hr_data = bind_to_handler(
                shell_item,
                ptr::null_mut(),
                &BHID_DATAOBJECT,
                &IID_IDATAOBJECT,
                &mut data_object,
            );
            if hr_data != 0 || data_object.is_null() {
                call_release(shell_item);
                return Err(format!("Failed to bind data object: {:#x}", hr_data));
            }

            // 2. Find target handler by index
            let mut enum_handlers: *mut c_void = ptr::null_mut();
            let hr_enum = SHAssocEnumHandlers(ext_wide.as_ptr(), 1, &mut enum_handlers);
            if hr_enum != 0 || enum_handlers.is_null() {
                call_release(data_object);
                call_release(shell_item);
                return Err(format!("Failed to enumerate handlers: {:#x}", hr_enum));
            }

            let enum_vt = *(enum_handlers as VTable);
            let next_fn: extern "system" fn(*mut c_void, u32, *mut *mut c_void, *mut u32) -> i32 =
                std::mem::transmute(*enum_vt.add(3));

            let mut cur_idx = 0;
            let mut launched = false;
            let mut last_err = 0;

            loop {
                let mut handler: *mut c_void = ptr::null_mut();
                let mut fetched = 0u32;
                let r = next_fn(enum_handlers, 1, &mut handler, &mut fetched);
                if r != 0 || fetched == 0 || handler.is_null() {
                    break;
                }

                if cur_idx == target_idx {
                    let h_vt = *(handler as VTable);
                    let invoke_fn: extern "system" fn(*mut c_void, *mut c_void) -> i32 =
                        std::mem::transmute(*h_vt.add(8));

                    let hr_inv = invoke_fn(handler, data_object);
                    if hr_inv == 0 {
                        launched = true;
                    } else {
                        last_err = hr_inv;
                    }
                    call_release(handler);
                    break;
                }

                cur_idx += 1;
                call_release(handler);
            }

            call_release(enum_handlers);
            call_release(data_object);
            call_release(shell_item);

            if launched {
                Ok(())
            } else {
                Err(format!(
                    "Could not launch application (code: {:#x})",
                    last_err
                ))
            }
        }
    }

    pub fn launch_open_with_dialog(file_path: &str) {
        use std::process::Command;
        let win_path = file_path.replace('/', "\\");
        let _ = Command::new("rundll32.exe")
            .args(["shell32.dll,OpenAs_RunDLL", &win_path])
            .spawn();
    }
}

#[cfg(target_os = "macos")]
mod macos_impl {
    use super::OpenWithItem;
    use std::path::Path;
    use std::process::Command;

    pub fn get_handlers_for_file(_file_path: &str) -> Vec<OpenWithItem> {
        let mut items = Vec::new();
        let mut idx = 0;

        let standard_apps = [
            ("Preview", "/System/Applications/Preview.app"),
            ("Photos", "/System/Applications/Photos.app"),
            ("Safari", "/Applications/Safari.app"),
            ("GIMP", "/Applications/GIMP.app"),
            ("Affinity Photo", "/Applications/Affinity Photo.app"),
        ];

        for (name, p) in &standard_apps {
            if Path::new(p).exists() {
                items.push(OpenWithItem {
                    id: idx,
                    name: name.to_string(),
                    icon_rgba: None,
                    icon_width: 0,
                    icon_height: 0,
                });
                idx += 1;
            }
        }

        items
    }

    pub fn launch_handler(file_path: &str, target_idx: i32) -> Result<(), String> {
        let standard_apps = [
            ("Preview", "/System/Applications/Preview.app"),
            ("Photos", "/System/Applications/Photos.app"),
            ("Safari", "/Applications/Safari.app"),
            ("GIMP", "/Applications/GIMP.app"),
            ("Affinity Photo", "/Applications/Affinity Photo.app"),
        ];
        let mut idx = 0;
        for (name, p) in &standard_apps {
            if Path::new(p).exists() {
                if idx == target_idx {
                    let res = Command::new("open").args(["-a", p, file_path]).spawn();
                    return match res {
                        Ok(_) => Ok(()),
                        Err(e) => Err(format!("Failed to launch {}: {}", name, e)),
                    };
                }
                idx += 1;
            }
        }
        Err("Application not found".to_string())
    }

    pub fn launch_open_with_dialog(file_path: &str) {
        let _ = Command::new("open").arg(file_path).spawn();
    }
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
mod unix_impl {
    use super::OpenWithItem;

    pub fn get_handlers_for_file(_file_path: &str) -> Vec<OpenWithItem> {
        Vec::new()
    }

    pub fn launch_handler(_file_path: &str, _target_idx: i32) -> Result<(), String> {
        Err("Unsupported on this platform".to_string())
    }

    pub fn launch_open_with_dialog(file_path: &str) {
        let _ = std::process::Command::new("xdg-open")
            .arg(file_path)
            .spawn();
    }
}

#[cfg(target_os = "windows")]
pub use win_impl::*;

#[cfg(target_os = "macos")]
pub use macos_impl::*;

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
pub use unix_impl::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_handlers_for_image_file() {
        let handlers = get_handlers_for_file("test.jpg");
        let _ = &handlers;
        #[cfg(target_os = "windows")]
        {
            println!("Found handlers for .jpg: {:?}", handlers);
            assert!(
                !handlers.is_empty(),
                "Expected at least 1 registered handler for .jpg"
            );
        }
    }
}
