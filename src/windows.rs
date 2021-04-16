use std::ffi::OsString;
use std::ffi::{OsStr, OsString};
use std::mem::MaybeUninit;
use std::ops::DerefMut;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::prelude::*;
use std::path::{Path, PathBuf};

use scopeguard::defer;

use winapi::DEFINE_GUID;
use winapi::{
    ctypes::{c_int, c_void},
    shared::guiddef::REFIID,
    shared::minwindef::UINT,
    shared::minwindef::{DWORD, FILETIME, LPVOID},
    shared::windef::HWND,
    shared::winerror::S_OK,
    shared::winerror::{HRESULT_FROM_WIN32, SUCCEEDED, S_OK},
    shared::wtypes::{VT_BSTR, VT_DATE},
    um::combaseapi::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL},
    um::errhandlingapi::GetLastError,
    um::minwinbase::SYSTEMTIME,
    um::oaidl::VARIANT,
    um::objbase::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, COINIT_MULTITHREADED,
        COINIT_SPEED_OVER_MEMORY,
    },
    um::oleauto::{VariantChangeType, VariantClear, VariantTimeToSystemTime},
    um::shellapi::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_SILENT, FOF_WANTNUKEWARNING, FO_DELETE,
        SHFILEOPSTRUCTW,
    },
    um::shellapi::{FOF_ALLOWUNDO, FOF_NO_UI, FOF_WANTNUKEWARNING},
    um::shlobj::CSIDL_BITBUCKET,
    um::shlwapi::StrRetToStrW,
    um::shobjidl_core::{
        FileOperation, IEnumIDList, IFileOperation, IShellFolder, IShellFolder2, IShellItem,
        SHCreateItemFromParsingName, SHCreateItemWithParent, FOFX_EARLYFAILURE, SHCONTF_FOLDERS,
        SHCONTF_NONFOLDERS, SHGDNF, SHGDN_FORPARSING, SHGDN_INFOLDER,
    },
    um::shtypes::{
        PCUITEMID_CHILD, PIDLIST_ABSOLUTE, PIDLIST_RELATIVE, PITEMID_CHILD, SHCOLUMNID, STRRET,
    },
    um::timezoneapi::SystemTimeToFileTime,
    um::winnt::PCZZWSTR,
    um::winnt::{PWSTR, ULARGE_INTEGER},
    Class, Interface,
};

use crate::{Error, ErrorKind, TrashItem};

macro_rules! return_err_on_fail {
    {$f_name:ident($($args:tt)*)} => ({
        let hr = $f_name($($args)*);
        if !SUCCEEDED(hr) {
            return Err(Error::kind_only(ErrorKind::PlatformApi {
                function_name: stringify!($f_name).into(),
                code: Some(hr)
            }));
        }
        hr
    });
    {$obj:ident.$f_name:ident($($args:tt)*)} => ({
        return_err_on_fail!{($obj).$f_name($($args)*)}
    });
    {($obj:expr).$f_name:ident($($args:tt)*)} => ({
        let hr = ($obj).$f_name($($args)*);
        if !SUCCEEDED(hr) {
            return Err(Error::kind_only(ErrorKind::PlatformApi {
                function_name: stringify!($f_name).into(),
                code: Some(hr)
            }));
        }
        hr
    })
}

/// See https://docs.microsoft.com/en-us/windows/win32/api/shellapi/ns-shellapi-_shfileopstructa
pub fn delete_all_canonicalized(full_paths: Vec<PathBuf>) -> Result<(), Error> {
    ensure_com_initialized();
    unsafe {
        let mut recycle_bin = MaybeUninit::<*mut IShellFolder2>::uninit();
        bind_to_csidl(
            CSIDL_BITBUCKET,
            &IShellFolder2::uuidof() as *const _,
            recycle_bin.as_mut_ptr() as *mut *mut c_void,
        )?;
        let recycle_bin = recycle_bin.assume_init();
        defer! {{ (*recycle_bin).Release(); }}
        // let mut pbc = MaybeUninit::<*mut IBindCtx>::uninit();
        // return_err_on_fail! { CreateBindCtx(0, pbc.as_mut_ptr()) };
        // let pbc = pbc.assume_init();
        // defer! {{ (*pbc).Release(); }}
        // (*pbc).
        let mut pfo = MaybeUninit::<*mut IFileOperation>::uninit();
        return_err_on_fail! {
            CoCreateInstance(
                &FileOperation::uuidof() as *const _,
                std::ptr::null_mut(),
                CLSCTX_ALL,
                &IFileOperation::uuidof() as *const _,
                pfo.as_mut_ptr() as *mut *mut c_void,
            )
        };
        let pfo = pfo.assume_init();
        defer! {{ (*pfo).Release(); }}
        return_err_on_fail! { (*pfo).SetOperationFlags(
            FOF_NO_UI as DWORD | FOF_ALLOWUNDO as DWORD | FOF_WANTNUKEWARNING as DWORD
        )};
        for full_path in full_paths.iter() {
            let path_prefix = ['\\' as u16, '\\' as u16, '?' as u16, '\\' as u16];
            let wide_path_container: Vec<_> =
                full_path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
            let wide_path_slice = if wide_path_container.starts_with(&path_prefix) {
                &wide_path_container[path_prefix.len()..]
            } else {
                &wide_path_container[0..]
            };
            let mut shi = MaybeUninit::<*mut IShellItem>::uninit();
            return_err_on_fail! {
                SHCreateItemFromParsingName(
                    wide_path_slice.as_ptr(),
                    std::ptr::null_mut(),
                    &IShellItem::uuidof() as *const _,
                    shi.as_mut_ptr() as *mut *mut c_void,
                )
            };
            let shi = shi.assume_init();
            defer! {{ (*shi).Release(); }}
            return_err_on_fail! { (*pfo).DeleteItem(shi, std::ptr::null_mut()) };
        }
        return_err_on_fail! { (*pfo).PerformOperations() };
        Ok(())
    }
}

pub fn list() -> Result<Vec<TrashItem>, Error> {
    ensure_com_initialized();
    unsafe {
        let mut recycle_bin = MaybeUninit::<*mut IShellFolder2>::uninit();
        bind_to_csidl(
            CSIDL_BITBUCKET,
            &IShellFolder2::uuidof() as *const _,
            recycle_bin.as_mut_ptr() as *mut *mut c_void,
        )?;
        let recycle_bin = recycle_bin.assume_init();
        defer! {{ (*recycle_bin).Release(); }};
        let mut peidl = MaybeUninit::<*mut IEnumIDList>::uninit();
        let hr = return_err_on_fail! {
            (*recycle_bin).EnumObjects(
                std::ptr::null_mut(),
                SHCONTF_FOLDERS | SHCONTF_NONFOLDERS,
                peidl.as_mut_ptr(),
            )
        };
        if hr != S_OK {
            return Err(Error::kind_only(ErrorKind::PlatformApi {
                function_name: "EnumObjects",
                code: Some(hr),
            }));
        }
        let peidl = peidl.assume_init();
        let mut item_vec = Vec::new();
        let mut item_uninit = MaybeUninit::<PITEMID_CHILD>::uninit();
        while (*peidl).Next(1, item_uninit.as_mut_ptr(), std::ptr::null_mut()) == S_OK {
            let item = item_uninit.assume_init();
            defer! {{ CoTaskMemFree(item as LPVOID); }}
            let id = get_display_name(recycle_bin as *mut _, item, SHGDN_FORPARSING)?;
            let name = get_display_name(recycle_bin as *mut _, item, SHGDN_INFOLDER)?;

            let orig_loc = get_detail(recycle_bin, item, &SCID_ORIGINAL_LOCATION as *const _)?;
            let date_deleted = get_date_unix(recycle_bin, item, &SCID_DATE_DELETED as *const _)?;

            item_vec.push(TrashItem {
                id,
                name: name.into_string().map_err(|original| {
                    Error::kind_only(ErrorKind::ConvertOsString { original })
                })?,
                original_parent: PathBuf::from(orig_loc),
                time_deleted: date_deleted,
            });
        }
        return Ok(item_vec);
    }
}

pub fn purge_all<I>(items: I) -> Result<(), Error>
where
    I: IntoIterator<Item = TrashItem>,
{
    ensure_com_initialized();
    unsafe {
        let mut recycle_bin = MaybeUninit::<*mut IShellFolder2>::uninit();
        bind_to_csidl(
            CSIDL_BITBUCKET,
            &IShellFolder2::uuidof() as *const _,
            recycle_bin.as_mut_ptr() as *mut *mut c_void,
        )?;
        let recycle_bin = recycle_bin.assume_init();
        defer! {{ (*recycle_bin).Release(); }}
        let mut pfo = MaybeUninit::<*mut IFileOperation>::uninit();
        return_err_on_fail! {
            CoCreateInstance(
                &FileOperation::uuidof() as *const _,
                std::ptr::null_mut(),
                CLSCTX_ALL,
                &IFileOperation::uuidof() as *const _,
                pfo.as_mut_ptr() as *mut *mut c_void,
            )
        };
        let pfo = pfo.assume_init();
        defer! {{ (*pfo).Release(); }}
        return_err_on_fail! { (*pfo).SetOperationFlags(FOF_NO_UI as DWORD) };
        let mut at_least_one = false;
        for item in items {
            at_least_one = true;
            let mut id_wstr: Vec<_> = item.id.encode_wide().chain(std::iter::once(0)).collect();
            let mut pidl = MaybeUninit::<PIDLIST_RELATIVE>::uninit();
            return_err_on_fail! {
                (*recycle_bin).ParseDisplayName(
                    0 as _,
                    std::ptr::null_mut(),
                    id_wstr.as_mut_ptr(),
                    std::ptr::null_mut(),
                    pidl.as_mut_ptr(),
                    std::ptr::null_mut(),
                )
            };
            let pidl = pidl.assume_init();
            defer! {{ CoTaskMemFree(pidl as LPVOID); }}
            let mut shi = MaybeUninit::<*mut IShellItem>::uninit();
            return_err_on_fail! {
                SHCreateItemWithParent(
                    std::ptr::null_mut(),
                    recycle_bin as *mut _,
                    pidl,
                    &IShellItem::uuidof() as *const _,
                    shi.as_mut_ptr() as *mut *mut c_void,
                )
            };
            let shi = shi.assume_init();
            defer! {{ (*shi).Release(); }}
            return_err_on_fail! { (*pfo).DeleteItem(shi, std::ptr::null_mut()) };
        }
        if at_least_one {
            return_err_on_fail! { (*pfo).PerformOperations() };
        }
        Ok(())
    }
}

pub fn restore_all<I>(items: I) -> Result<(), Error>
where
    I: IntoIterator<Item = TrashItem>,
{
    let items: Vec<_> = items.into_iter().collect();

    // Do a quick and dirty check if the target items already exist at the location
    // and if they do, return all of them, if they don't just go ahead with the processing
    // without giving a damn.
    // Note that this is not 'thread safe' meaning that if a paralell thread (or process)
    // does this operation the exact same time or creates files or folders right after this check,
    // then the files that would collide will not be detected and returned as part of an error.
    // Instead Windows will display a prompt to the user whether they want to replace or skip.
    for item in items.iter() {
        let path = item.original_path();
        if path.exists() {
            return Err(Error::kind_only(ErrorKind::RestoreCollision {
                path: path,
                remaining_items: items.into(),
            }));
        }
    }
    ensure_com_initialized();
    unsafe {
        let mut recycle_bin = MaybeUninit::<*mut IShellFolder2>::uninit();
        bind_to_csidl(
            CSIDL_BITBUCKET,
            &IShellFolder2::uuidof() as *const _,
            recycle_bin.as_mut_ptr() as *mut *mut c_void,
        )?;
        let recycle_bin = recycle_bin.assume_init();
        defer! {{ (*recycle_bin).Release(); }}
        let mut pfo = MaybeUninit::<*mut IFileOperation>::uninit();
        return_err_on_fail! {
            CoCreateInstance(
                &FileOperation::uuidof() as *const _,
                std::ptr::null_mut(),
                CLSCTX_ALL,
                &IFileOperation::uuidof() as *const _,
                pfo.as_mut_ptr() as *mut *mut c_void,
            )
        };
        let pfo = pfo.assume_init();
        defer! {{ (*pfo).Release(); }}
        return_err_on_fail! { (*pfo).SetOperationFlags(FOF_NO_UI as DWORD | FOFX_EARLYFAILURE) };
        for item in items.iter() {
            let mut id_wstr: Vec<_> = item.id.encode_wide().chain(std::iter::once(0)).collect();
            let mut pidl = MaybeUninit::<PIDLIST_RELATIVE>::uninit();
            return_err_on_fail! {
                (*recycle_bin).ParseDisplayName(
                    0 as _,
                    std::ptr::null_mut(),
                    id_wstr.as_mut_ptr(),
                    std::ptr::null_mut(),
                    pidl.as_mut_ptr(),
                    std::ptr::null_mut(),
                )
            };
            let pidl = pidl.assume_init();
            defer! {{ CoTaskMemFree(pidl as LPVOID); }}
            let mut trash_item_shi = MaybeUninit::<*mut IShellItem>::uninit();
            return_err_on_fail! {
                SHCreateItemWithParent(
                    std::ptr::null_mut(),
                    recycle_bin as *mut _,
                    pidl,
                    &IShellItem::uuidof() as *const _,
                    trash_item_shi.as_mut_ptr() as *mut *mut c_void,
                )
            };
            let trash_item_shi = trash_item_shi.assume_init();
            defer! {{ (*trash_item_shi).Release(); }}
            let parent_path_wide: Vec<_> =
                item.original_parent.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
            let mut orig_folder_shi = MaybeUninit::<*mut IShellItem>::uninit();
            return_err_on_fail! {
                SHCreateItemFromParsingName(
                    parent_path_wide.as_ptr(),
                    std::ptr::null_mut(),
                    &IShellItem::uuidof() as *const _,
                    orig_folder_shi.as_mut_ptr() as *mut *mut c_void,
                )
            };
            let orig_folder_shi = orig_folder_shi.assume_init();
            defer! {{ (*orig_folder_shi).Release(); }}
            let name_wstr: Vec<_> = AsRef::<OsStr>::as_ref(&item.name)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            return_err_on_fail! { (*pfo).MoveItem(trash_item_shi, orig_folder_shi, name_wstr.as_ptr(), std::ptr::null_mut()) };
        }
        if items.len() > 0 {
            return_err_on_fail! { (*pfo).PerformOperations() };
        }
        Ok(())
    }
}

struct CoInitializer {}
impl CoInitializer {
    fn new() -> CoInitializer {
        //let first = INITIALIZER_THREAD_COUNT.fetch_add(1, Ordering::SeqCst) == 0;
        let mut init_mode = 0;
        if cfg!(coinit_multithreaded) {
            init_mode |= COINIT_MULTITHREADED;
        } else if cfg!(coinit_apartmentthreaded) {
            init_mode |= COINIT_APARTMENTTHREADED;
        }
        // `else` missing intentionaly. These flags can be combined.
        if cfg!(coinit_disable_ole1dde) {
            init_mode |= COINIT_DISABLE_OLE1DDE;
        }
        if cfg!(coinit_speed_over_memory) {
            init_mode |= COINIT_SPEED_OVER_MEMORY;
        }
        let hr = unsafe { CoInitializeEx(std::ptr::null_mut(), init_mode) };
        if !SUCCEEDED(hr) {
            panic!(format!("Call to CoInitializeEx failed. HRESULT: {:X}. Consider using `trash` with the feature `coinit_multithreaded`", hr));
        }
        CoInitializer {}
    }
}
impl Drop for CoInitializer {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
thread_local! {
    static CO_INITIALIZER: CoInitializer = CoInitializer::new();
}
fn ensure_com_initialized() {
    CO_INITIALIZER.with(|_| {});
}

unsafe fn bind_to_csidl(csidl: c_int, riid: REFIID, ppv: *mut *mut c_void) -> Result<(), Error> {
    use winapi::um::shlobj::{SHGetDesktopFolder, SHGetSpecialFolderLocation};

    let mut pidl = MaybeUninit::<PIDLIST_ABSOLUTE>::uninit();
    return_err_on_fail! {
        SHGetSpecialFolderLocation(std::ptr::null_mut(), csidl, pidl.as_mut_ptr())
    };
    let pidl = pidl.assume_init();
    defer! {{ CoTaskMemFree(pidl as LPVOID); }};
    let mut desktop = MaybeUninit::<*mut IShellFolder>::uninit();
    return_err_on_fail! {SHGetDesktopFolder(desktop.as_mut_ptr() as *mut *mut _)};
    let desktop = desktop.assume_init();
    defer! {{ (*desktop).Release(); }};
    if (*pidl).mkid.cb != 0 {
        return_err_on_fail! {(*desktop).BindToObject(pidl, std::ptr::null_mut(), riid, ppv)};
    } else {
        return_err_on_fail! {(*desktop).QueryInterface(riid, ppv)};
    }
    Ok(())
}

unsafe fn wstr_to_os_string(wstr: PWSTR) -> OsString {
    let mut len = 0;
    while *(wstr.offset(len)) != 0 {
        len += 1;
    }
    let wstr_slice = std::slice::from_raw_parts(wstr, len as usize);
    OsString::from_wide(wstr_slice)
}

unsafe fn get_display_name(
    psf: *mut IShellFolder,
    pidl: PCUITEMID_CHILD,
    flags: SHGDNF,
) -> Result<OsString, Error> {
    let mut sr = MaybeUninit::<STRRET>::uninit();
    return_err_on_fail! {(*psf).GetDisplayNameOf(pidl, flags, sr.as_mut_ptr())};
    let mut sr = sr.assume_init();
    let mut name = MaybeUninit::<PWSTR>::uninit();
    return_err_on_fail! {StrRetToStrW(&mut sr as *mut _, pidl, name.as_mut_ptr())};
    let name = name.assume_init();
    let result = wstr_to_os_string(name);
    CoTaskMemFree(name as LPVOID);
    Ok(result)
}

unsafe fn get_detail(
    psf: *mut IShellFolder2,
    pidl: PCUITEMID_CHILD,
    pscid: *const SHCOLUMNID,
) -> Result<OsString, Error> {
    let mut vt = MaybeUninit::<VARIANT>::uninit();
    return_err_on_fail! { (*psf).GetDetailsEx(pidl, pscid, vt.as_mut_ptr()) };
    let vt = vt.assume_init();
    let mut vt = scopeguard::guard(vt, |mut vt| {
        VariantClear(&mut vt as *mut _);
    });
    //defer! {{ VariantClear(&mut vt as *mut _); }};
    return_err_on_fail! {
        VariantChangeType(vt.deref_mut() as *mut _, vt.deref_mut() as *mut _, 0, VT_BSTR as u16)
    };
    let a = vt.n1.n2().n3.bstrVal();
    let result = Ok(wstr_to_os_string(*a));
    return result;
}

fn windows_ticks_to_unix_seconds(windows_ticks: u64) -> i64 {
    const WINDOWS_TICK: u64 = 10000000;
    const SEC_TO_UNIX_EPOCH: i64 = 11644473600;
    return (windows_ticks / WINDOWS_TICK) as i64 - SEC_TO_UNIX_EPOCH;
}

unsafe fn variant_time_to_unix_time(from: f64) -> Result<i64, Error> {
    let mut st = MaybeUninit::<SYSTEMTIME>::uninit();
    return_err_on_fail! { VariantTimeToSystemTime(from, st.as_mut_ptr()) };
    let st = st.assume_init();
    let mut ft = MaybeUninit::<FILETIME>::uninit();
    if SystemTimeToFileTime(&st, ft.as_mut_ptr()) == 0 {
        return Err(Error::kind_only(ErrorKind::PlatformApi {
            function_name: "SystemTimeToFileTime",
            code: Some(HRESULT_FROM_WIN32(GetLastError())),
        }));
    }
    let ft = ft.assume_init();
    // Applying assume init straight away because there's no explicit support to initialize struct
    // fields one-by-one in an `MaybeUninit` as of Rust 1.39.0
    // See: https://github.com/rust-lang/rust/blob/1.39.0/src/libcore/mem/maybe_uninit.rs#L170
    let mut uli = MaybeUninit::<ULARGE_INTEGER>::zeroed().assume_init();
    {
        let u_mut = uli.u_mut();
        u_mut.LowPart = ft.dwLowDateTime;
        u_mut.HighPart = std::mem::transmute(ft.dwHighDateTime);
    }
    let windows_ticks: u64 = *uli.QuadPart();
    Ok(windows_ticks_to_unix_seconds(windows_ticks))
}

unsafe fn get_date_unix(
    psf: *mut IShellFolder2,
    pidl: PCUITEMID_CHILD,
    pscid: *const SHCOLUMNID,
) -> Result<i64, Error> {
    let mut vt = MaybeUninit::<VARIANT>::uninit();
    return_err_on_fail! { (*psf).GetDetailsEx(pidl, pscid, vt.as_mut_ptr()) };
    let vt = vt.assume_init();
    let mut vt = scopeguard::guard(vt, |mut vt| {
        VariantClear(&mut vt as *mut _);
    });
    return_err_on_fail! {
        VariantChangeType(vt.deref_mut() as *mut _, vt.deref_mut() as *mut _, 0, VT_DATE as u16)
    };
    let date = *vt.n1.n2().n3.date();
    let unix_time = variant_time_to_unix_time(date)?;
    Ok(unix_time)
}

DEFINE_GUID! {
    PSGUID_DISPLACED,
    0x9b174b33, 0x40ff, 0x11d2, 0xa2, 0x7e, 0x00, 0xc0, 0x4f, 0xc3, 0x8, 0x71
}

const PID_DISPLACED_FROM: DWORD = 2;
const PID_DISPLACED_DATE: DWORD = 3;

const SCID_ORIGINAL_LOCATION: SHCOLUMNID =
    SHCOLUMNID { fmtid: PSGUID_DISPLACED, pid: PID_DISPLACED_FROM };
const SCID_DATE_DELETED: SHCOLUMNID =
    SHCOLUMNID { fmtid: PSGUID_DISPLACED, pid: PID_DISPLACED_DATE };
