//! Per-user shell verbs for file system folders and drives.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS,
    HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT, WIN32_ERROR,
};
use windows_sys::Win32::Storage::FileSystem::DELETE;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS, REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
    RegSetValueExW,
};
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
use windows_sys::core::{PCWSTR, w};

const CLASSES: PCWSTR = w!(r"Software\Classes");
const SHELLS: [PCWSTR; 2] = [w!(r"Directory\shell"), w!(r"Drive\shell")];
const VERB: &str = "8823.OpenFolder";
const VERB_KEY: PCWSTR = w!("8823.OpenFolder");
const PREVIOUS: PCWSTR = w!("PreviousDefault");
const DEFAULT: PCWSTR = null();
const UPDATE: REG_SAM_FLAGS = KEY_READ | KEY_WRITE;
const CLEANUP: REG_SAM_FLAGS = UPDATE | DELETE;

pub const OPEN_FLAG: &str = "--open-folder";

#[derive(Default)]
pub struct Status {
    pub selected: usize,
    pub can_restore: bool,
}

pub fn status() -> io::Result<Status> {
    let Some(classes) = Key::open(HKEY_CURRENT_USER, CLASSES, KEY_READ)? else {
        return Ok(Status::default());
    };
    status_in(classes.0)
}

pub fn set_default(enabled: bool) -> io::Result<()> {
    let _lock = RegistrationLock::acquire()?;
    let result = if enabled {
        let command = launch_command(&std::env::current_exe()?)?;
        let classes = Key::create(HKEY_CURRENT_USER, CLASSES, UPDATE)?;
        register_in(classes.0, &command)
    } else if let Some(classes) = Key::open(HKEY_CURRENT_USER, CLASSES, KEY_READ)? {
        restore_in(classes.0)
    } else {
        Ok(())
    };
    // SAFETY: Association-change notifications take no item pointers.
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, null(), null()) };
    result
}

pub fn shell_path(arg: OsString) -> Option<PathBuf> {
    let wide: Vec<u16> = arg.encode_wide().collect();
    let path = PathBuf::from(OsString::from_wide(
        wide.strip_suffix(&[b'\\' as u16, b'.' as u16])
            .unwrap_or(&wide),
    ));
    path.is_absolute().then_some(path)
}

fn launch_command(exe: &Path) -> io::Result<Value> {
    if exe
        .as_os_str()
        .encode_wide()
        .any(|ch| ch == u16::from(b'%') || ch == u16::from(b'"'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Use an 8823.exe path without % or quotation marks",
        ));
    }
    let mut command = OsString::from("\"");
    command.push(exe);
    // The dot keeps a drive root's backslash away from the closing quote.
    command.push(format!(r#"" {OPEN_FLAG} "%1\.""#));
    Ok(Value::text(&command))
}

fn status_in(root: HKEY) -> io::Result<Status> {
    let mut status = Status::default();
    for path in SHELLS {
        let Some(shell) = Key::open(root, path, KEY_READ)? else {
            continue;
        };
        let selected = shell.get(DEFAULT)?.is_some_and(|value| value.is_our_verb());
        status.selected += usize::from(selected);
        status.can_restore |= selected || Key::open(shell.0, VERB_KEY, KEY_READ)?.is_some();
    }
    Ok(status)
}

fn register_in(root: HKEY, command: &Value) -> io::Result<()> {
    let shells = [
        Key::create(root, SHELLS[0], CLEANUP)?,
        Key::create(root, SHELLS[1], CLEANUP)?,
    ];
    let previous = [shells[0].get(DEFAULT)?, shells[1].get(DEFAULT)?];
    let verbs = [
        Key::create(shells[0].0, VERB_KEY, CLEANUP)?,
        Key::create(shells[1].0, VERB_KEY, CLEANUP)?,
    ];
    let commands = [
        Key::create(verbs[0].0, w!("command"), CLEANUP)?,
        Key::create(verbs[1].0, w!("command"), CLEANUP)?,
    ];

    // Prepare both launch commands and their backups before changing either default.
    for ((verb, command_key), value) in verbs.iter().zip(&commands).zip(&previous) {
        if !value.as_ref().is_some_and(Value::is_our_verb) {
            verb.replace(PREVIOUS, value.as_ref())?;
        }
        command_key.replace(DEFAULT, Some(command))?;
        verb.replace(DEFAULT, Some(&Value::text(OsStr::new("Open in 8823"))))?;
    }
    let selected = Value::text(OsStr::new(VERB));
    shells[0].replace(DEFAULT, Some(&selected))?;
    if let Err(error) = shells[1].replace(DEFAULT, Some(&selected)) {
        if let Err(rollback) = restore_default(&shells[0], previous[0].as_ref()) {
            return Err(io::Error::other(format!(
                "Could not set the drive default ({error}). Restoring the folder default also failed ({rollback})."
            )));
        }
        return Err(error);
    }
    Ok(())
}

fn restore_in(root: HKEY) -> io::Result<()> {
    let mut error = None;
    for path in SHELLS {
        if let Err(err) = restore_shell(root, path) {
            error.get_or_insert(err);
        }
    }
    error.map_or(Ok(()), Err)
}

fn restore_shell(root: HKEY, path: PCWSTR) -> io::Result<()> {
    let Some(shell) = Key::open(root, path, UPDATE)? else {
        return Ok(());
    };
    let (registered, previous) = match Key::open(shell.0, VERB_KEY, KEY_READ)? {
        Some(verb) => (true, verb.get(PREVIOUS)?),
        None => (false, None),
    };
    restore_default(&shell, previous.as_ref())?;

    // Request deletion rights after restoring the default, in case the ACL changed.
    if !registered {
        return Ok(());
    }
    let Some(shell) = Key::open(root, path, CLEANUP)? else {
        return Ok(());
    };
    // SAFETY: The open shell key is live and this removes only 8823's own verb.
    allow_missing(unsafe { RegDeleteTreeW(shell.0, VERB_KEY) })
}

fn restore_default(shell: &Key, previous: Option<&Value>) -> io::Result<()> {
    // A different application may have taken over after the user registered 8823.
    if shell.get(DEFAULT)?.is_some_and(|value| value.is_our_verb()) {
        shell.replace(DEFAULT, previous)?;
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct Value {
    kind: u32,
    bytes: Vec<u8>,
}

impl Value {
    fn text(value: &OsStr) -> Self {
        Self {
            kind: REG_SZ,
            bytes: value
                .encode_wide()
                .chain(Some(0))
                .flat_map(u16::to_le_bytes)
                .collect(),
        }
    }

    fn is_our_verb(&self) -> bool {
        if self.kind != REG_SZ {
            return false;
        }
        let wide: Vec<u16> = self
            .bytes
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .take_while(|&unit| unit != 0)
            .collect();
        String::from_utf16_lossy(&wide).eq_ignore_ascii_case(VERB)
    }
}

struct Key(HKEY);

impl Key {
    fn open(root: HKEY, path: PCWSTR, access: REG_SAM_FLAGS) -> io::Result<Option<Self>> {
        let mut key = null_mut();
        // SAFETY: The root is live, path is terminated, and key is writable.
        match unsafe { RegOpenKeyExW(root, path, 0, access, &mut key) } {
            ERROR_SUCCESS => Ok(Some(Self(key))),
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(None),
            error => Err(io::Error::from_raw_os_error(error as i32)),
        }
    }

    fn create(root: HKEY, path: PCWSTR, access: REG_SAM_FLAGS) -> io::Result<Self> {
        let mut key = null_mut();
        // SAFETY: The root is live, path is terminated, and the optional outputs are null.
        check(unsafe {
            RegCreateKeyExW(
                root,
                path,
                0,
                null(),
                REG_OPTION_NON_VOLATILE,
                access,
                null(),
                &mut key,
                null_mut(),
            )
        })?;
        Ok(Self(key))
    }

    fn get(&self, name: PCWSTR) -> io::Result<Option<Value>> {
        let mut kind = 0;
        let mut bytes = Vec::new();
        loop {
            let mut len = bytes.len() as u32;
            let data = if bytes.is_empty() {
                null_mut()
            } else {
                bytes.as_mut_ptr()
            };
            // SAFETY: The key is live, name is terminated or null, and data has len writable bytes.
            let result =
                unsafe { RegQueryValueExW(self.0, name, null(), &mut kind, data, &mut len) };
            match result {
                ERROR_SUCCESS if len as usize <= bytes.len() => {
                    bytes.truncate(len as usize);
                    return Ok(Some(Value { kind, bytes }));
                }
                ERROR_SUCCESS | ERROR_MORE_DATA => bytes.resize(len as usize, 0),
                ERROR_FILE_NOT_FOUND => return Ok(None),
                error => return Err(io::Error::from_raw_os_error(error as i32)),
            }
        }
    }

    fn replace(&self, name: PCWSTR, value: Option<&Value>) -> io::Result<()> {
        // SAFETY: The key is live, name is terminated or null, and the value owns its data.
        unsafe {
            match value {
                Some(value) => check(RegSetValueExW(
                    self.0,
                    name,
                    0,
                    value.kind,
                    value.bytes.as_ptr(),
                    value.bytes.len() as u32,
                )),
                None => allow_missing(RegDeleteValueW(self.0, name)),
            }
        }
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: Each Key owns one handle returned by a successful open or create.
        unsafe { RegCloseKey(self.0) };
    }
}

fn check(result: WIN32_ERROR) -> io::Result<()> {
    if result == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(result as i32))
    }
}

fn allow_missing(result: WIN32_ERROR) -> io::Result<()> {
    if matches!(result, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) {
        Ok(())
    } else {
        check(result)
    }
}

struct RegistrationLock(HANDLE);

impl RegistrationLock {
    fn acquire() -> io::Result<Self> {
        // SAFETY: The mutex is created with default security and a terminated name.
        let handle = unsafe { CreateMutexW(null(), 0, w!(r"Local\8823.FolderApp")) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: The handle is open and closed below if the lock was not acquired.
        let error = match unsafe { WaitForSingleObject(handle, 0) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => return Ok(Self(handle)),
            WAIT_TIMEOUT => io::Error::new(
                io::ErrorKind::WouldBlock,
                "Another 8823 window is changing folder settings",
            ),
            _ => io::Error::last_os_error(),
        };
        unsafe { CloseHandle(handle) };
        Err(error)
    }
}

impl Drop for RegistrationLock {
    fn drop(&mut self) {
        // SAFETY: This thread owns the mutex, and the handle is still open.
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

    struct TestClasses {
        key: Key,
        path: Vec<u16>,
    }

    impl TestClasses {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path: Vec<u16> = format!(
                r"Software\T11ii.8823.FolderAppTest-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            )
            .encode_utf16()
            .chain(Some(0))
            .collect();
            let key = Key::create(HKEY_CURRENT_USER, path.as_ptr(), UPDATE | DELETE).unwrap();
            Self { key, path }
        }
    }

    impl Drop for TestClasses {
        fn drop(&mut self) {
            // SAFETY: This path identifies the private test tree created above.
            unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, self.path.as_ptr()) };
        }
    }

    #[test]
    fn re_registering_then_restoring_recovers_an_existing_or_inherited_default() {
        let classes = TestClasses::new();
        let root = classes.key.0;
        let directory = Key::create(root, SHELLS[0], UPDATE).unwrap();
        let previous = Value::text(OsStr::new("FilePilot.Open"));
        directory.replace(DEFAULT, Some(&previous)).unwrap();

        for exe in [r"C:\Apps\8823.exe", r"C:\My Tools\8823.exe"] {
            register_in(root, &launch_command(Path::new(exe)).unwrap()).unwrap();
            assert_eq!(status_in(root).unwrap().selected, 2);
        }
        restore_in(root).unwrap();
        assert_eq!(directory.get(DEFAULT).unwrap(), Some(previous));
        let drive = Key::open(root, SHELLS[1], KEY_READ).unwrap().unwrap();
        assert_eq!(drive.get(DEFAULT).unwrap(), None);
        assert!(!status_in(root).unwrap().can_restore);
        restore_in(root).unwrap();
    }

    #[test]
    fn restoring_preserves_a_default_selected_by_another_app() {
        let classes = TestClasses::new();
        let root = classes.key.0;
        register_in(
            root,
            &launch_command(Path::new(r"C:\Apps\8823.exe")).unwrap(),
        )
        .unwrap();
        let directory = Key::open(root, SHELLS[0], UPDATE).unwrap().unwrap();
        let other = Value::text(OsStr::new("OtherApp.Open"));
        directory.replace(DEFAULT, Some(&other)).unwrap();
        restore_in(root).unwrap();
        assert_eq!(directory.get(DEFAULT).unwrap(), Some(other));
        assert!(!status_in(root).unwrap().can_restore);
    }

    #[test]
    fn shell_command_preserves_drive_roots_and_paths_with_spaces() {
        let exe = r"C:\My Tools\8823.exe";
        let template = launch_command(Path::new(exe)).unwrap();
        let wide: Vec<u16> = template
            .bytes
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .collect();
        let template = String::from_utf16(wide.strip_suffix(&[0]).unwrap()).unwrap();
        for target in [r"D:\", r"\\server\share\研究 メモ"] {
            let command = template.replace("%1", target);
            let command: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
            let mut count = 0;
            // SAFETY: The command is NUL-terminated and count is an initialized output.
            let argv = unsafe { CommandLineToArgvW(command.as_ptr(), &mut count) };
            assert!(!argv.is_null());
            // SAFETY: argv contains count terminated argument strings and one allocation.
            let args: Vec<OsString> = unsafe {
                let args = (0..count)
                    .map(|i| {
                        let arg = *argv.add(i as usize);
                        let len = (0..).take_while(|&j| *arg.add(j) != 0).count();
                        OsString::from_wide(std::slice::from_raw_parts(arg, len))
                    })
                    .collect();
                LocalFree(argv.cast());
                args
            };
            assert_eq!(args.len(), 3);
            assert_eq!(args[0], exe);
            assert_eq!(args[1], OPEN_FLAG);
            assert_eq!(shell_path(args[2].clone()), Some(PathBuf::from(target)));
        }
    }
}
