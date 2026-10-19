//! The daemon's local transport (3): a named pipe on Windows, a Unix domain socket elsewhere,
//! reachable only by the current user.
//!
//! - **Windows**: `\\.\pipe\beacon-<key>-<random>`. The first instance is created with
//!   `FILE_FLAG_FIRST_PIPE_INSTANCE` (fails if anyone owns the name already: we pick another
//!   random suffix), every instance with `PIPE_REJECT_REMOTE_CLIENTS` and a protected DACL
//!   granting only the current user's SID (`D:P(A;;GA;;;<sid>)`). Clients open with
//!   `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`, so a squatting server cannot
//!   impersonate them. Handles are overlapped: synchronous pipe handles serialize reads and
//!   writes, which deadlocks a full-duplex relay.
//! - **Unix**: `<run dir>/<key>.sock` in a 0700 directory owned by the user and not a symlink
//!   ([`secure_dir`]); paths over the `sun_path` limit fall back to `/tmp/beacon-<uid>/`.

use std::io::{self, Read, Write};
use std::path::Path;

/// One connection, either side. `try_clone` gives an independent reader/writer.
pub struct Stream {
    #[cfg(windows)]
    h: std::os::windows::io::OwnedHandle,
    #[cfg(unix)]
    s: std::os::unix::net::UnixStream,
}

/// The daemon's accepting end.
pub struct Listener {
    #[cfg(windows)]
    name: Vec<u16>,
    #[cfg(windows)]
    sd: win::Sd,
    /// The instance the next client connects to (there is always one, so a client sees
    /// "busy", never "no such pipe", while the daemon lives).
    #[cfg(windows)]
    pending: std::os::windows::io::OwnedHandle,
    #[cfg(unix)]
    l: std::os::unix::net::UnixListener,
}

impl Stream {
    pub fn try_clone(&self) -> io::Result<Stream> {
        #[cfg(windows)]
        return Ok(Stream {
            h: self.h.try_clone()?,
        });
        #[cfg(unix)]
        return Ok(Stream {
            s: self.s.try_clone()?,
        });
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(windows)]
        return win::read(&self.h, buf);
        #[cfg(unix)]
        return self.s.read(buf);
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        #[cfg(windows)]
        return win::write(&self.h, buf);
        #[cfg(unix)]
        return self.s.write(buf);
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Listener {
    /// Creates the endpoint for `key`; returns it with its name (for the discovery file).
    /// The caller holds the project lock, so a leftover socket file is stale.
    pub fn bind(run: &Path, key: &str) -> io::Result<(Listener, String)> {
        #[cfg(windows)]
        let _ = run; // pipes live in the pipe namespace
        #[cfg(windows)]
        return win::bind(key);
        #[cfg(unix)]
        return unix::bind(run, key);
    }

    /// Blocks until the next client connects.
    pub fn accept(&mut self) -> io::Result<Stream> {
        #[cfg(windows)]
        return win::accept(self);
        #[cfg(unix)]
        return self.l.accept().map(|(s, _)| Stream { s });
    }
}

/// Connects to the endpoint named in a discovery file.
pub fn connect(name: &str) -> io::Result<Stream> {
    #[cfg(windows)]
    return win::connect(name);
    #[cfg(unix)]
    return std::os::unix::net::UnixStream::connect(name).map(|s| Stream { s });
}

/// Removes what [`Listener::bind`] left on disk (the socket file; nothing on Windows). Call
/// while still holding the project lock.
pub fn cleanup(name: &str) {
    if cfg!(unix) {
        let _ = std::fs::remove_file(name);
    }
}

/// Creates `dir` (mode 0700, the final component not followed if it is a symlink) and checks
/// it: a real directory, owned by the effective user, no group/other access. Parents are
/// created normally. On Windows the per-user profile directory is already private: created
/// only.
pub fn secure_dir(dir: &Path) -> io::Result<()> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    return unix::secure_dir(dir);
    #[cfg(windows)]
    return match std::fs::create_dir(dir) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => Err(e),
        _ => Ok(()),
    };
}

#[cfg(unix)]
mod unix {
    use super::Listener;
    use std::io;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};

    /// `sun_path` is 104 bytes on macOS (108 on Linux), NUL included.
    const SUN_PATH: usize = 104;

    pub(super) fn secure_dir(dir: &Path) -> io::Result<()> {
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
            _ => {}
        }
        // lstat: a symlink (even to a good directory) is refused, not followed
        let m = std::fs::symlink_metadata(dir)?;
        let refuse = |why: &str| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("refusing run directory {}: {why}", dir.display()),
            ))
        };
        if m.file_type().is_symlink() || !m.is_dir() {
            return refuse("not a plain directory");
        }
        // SAFETY: no arguments, cannot fail.
        if m.uid() != unsafe { libc::geteuid() } {
            return refuse("owned by another user");
        }
        if m.mode() & 0o077 != 0 {
            // ours, only too open (e.g. created by an older tool): tighten
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    pub(super) fn bind(run: &Path, key: &str) -> io::Result<(Listener, String)> {
        let mut path = run.join(format!("{key}.sock"));
        if path.as_os_str().len() >= SUN_PATH {
            // SAFETY: as above.
            let dir = PathBuf::from(format!("/tmp/beacon-{}", unsafe { libc::geteuid() }));
            secure_dir(&dir)?;
            path = dir.join(format!("{key}.sock"));
        }
        let _ = std::fs::remove_file(&path); // stale: we hold the lock
        let l = UnixListener::bind(&path)?;
        Ok((Listener { l }, path.to_string_lossy().into_owned()))
    }
}

#[cfg(windows)]
mod win {
    use super::{Listener, Stream};
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{
        ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
        ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
        INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        GetSecurityInfo, SDDL_REVISION_1, SE_KERNEL_OBJECT,
    };
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
        DACL_SECURITY_INFORMATION, GetAce, GetAclInformation, GetTokenInformation,
        PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
        PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
    };
    use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, WaitNamedPipeW,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, GetCurrentProcess, OpenProcessToken,
    };

    const BUF: u32 = 64 << 10;

    /// A security descriptor from `ConvertStringSecurityDescriptorToSecurityDescriptorW`.
    pub(super) struct Sd(PSECURITY_DESCRIPTOR);
    // SAFETY: an immutable heap block owned by this value only.
    unsafe impl Send for Sd {}
    impl Drop for Sd {
        fn drop(&mut self) {
            // SAFETY: allocated by the conversion with LocalAlloc.
            unsafe { LocalFree(self.0) };
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    fn owned(h: HANDLE) -> io::Result<OwnedHandle> {
        if h.is_null() || h == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: a fresh handle we own.
            Ok(unsafe { OwnedHandle::from_raw_handle(h) })
        }
    }

    /// Runs one overlapped operation to completion (each call has its own event, so a
    /// reader and a writer on clones of the same handle never wait for each other).
    fn overlapped(
        h: &OwnedHandle,
        op: impl FnOnce(HANDLE, *mut OVERLAPPED) -> i32,
    ) -> io::Result<u32> {
        // SAFETY: plain calls; `ov` outlives the operation because we wait for it below
        // whenever it is pending.
        unsafe {
            let ev = owned(CreateEventW(null(), 1, 0, null()))?;
            let mut ov: OVERLAPPED = std::mem::zeroed();
            ov.hEvent = ev.as_raw_handle();
            let raw = h.as_raw_handle();
            if op(raw, &mut ov) == 0 {
                let e = GetLastError();
                if e != ERROR_IO_PENDING {
                    return Err(io::Error::from_raw_os_error(e as i32));
                }
            }
            let mut n = 0;
            if GetOverlappedResult(raw, &ov, &mut n, 1) == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(n)
        }
    }

    pub(super) fn read(h: &OwnedHandle, buf: &mut [u8]) -> io::Result<usize> {
        let len = buf.len().min(u32::MAX as usize) as u32;
        // SAFETY: `buf` is valid for `len` bytes until the operation completes.
        match overlapped(h, |h, ov| unsafe {
            ReadFile(h, buf.as_mut_ptr(), len, null_mut(), ov)
        }) {
            Ok(n) => Ok(n as usize),
            // the other side closed: end of stream
            Err(e)
                if [ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED]
                    .contains(&(e.raw_os_error().unwrap_or(0) as u32)) =>
            {
                Ok(0)
            }
            Err(e) => Err(e),
        }
    }

    pub(super) fn write(h: &OwnedHandle, buf: &[u8]) -> io::Result<usize> {
        let len = buf.len().min(u32::MAX as usize) as u32;
        // SAFETY: as in `read`.
        overlapped(h, |h, ov| unsafe {
            WriteFile(h, buf.as_ptr(), len, null_mut(), ov)
        })
        .map(|n| n as usize)
    }

    fn instance(name: &[u16], sd: &Sd, first: bool) -> io::Result<OwnedHandle> {
        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: 0,
        };
        let open = PIPE_ACCESS_DUPLEX
            | FILE_FLAG_OVERLAPPED
            | if first {
                FILE_FLAG_FIRST_PIPE_INSTANCE
            } else {
                0
            };
        // SAFETY: valid NUL-terminated name and attributes for the duration of the call.
        owned(unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                open,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                BUF,
                BUF,
                0,
                &sa,
            )
        })
    }

    /// The current user's SID as a string (`S-1-5-21-...`).
    pub fn current_user_sid() -> io::Result<String> {
        // SAFETY: out-parameter calls on the current process token; the buffer is sized by
        // the first call and u64-aligned for TOKEN_USER.
        unsafe {
            let mut tok = null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) == 0 {
                return Err(io::Error::last_os_error());
            }
            let tok = owned(tok)?;
            let mut len = 0;
            GetTokenInformation(tok.as_raw_handle(), TokenUser, null_mut(), 0, &mut len);
            let mut buf = vec![0u64; (len as usize).div_ceil(8)];
            if GetTokenInformation(
                tok.as_raw_handle(),
                TokenUser,
                buf.as_mut_ptr().cast(),
                len,
                &mut len,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            sid_string((*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid)
        }
    }

    /// SAFETY: `sid` must point to a valid SID.
    unsafe fn sid_string(sid: *mut core::ffi::c_void) -> io::Result<String> {
        unsafe {
            let mut p = null_mut();
            if ConvertSidToStringSidW(sid, &mut p) == 0 {
                return Err(io::Error::last_os_error());
            }
            let n = (0..).take_while(|&i| *p.add(i) != 0).count();
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            LocalFree(p.cast());
            Ok(s)
        }
    }

    pub(super) fn bind(key: &str) -> io::Result<(Listener, String)> {
        let sddl = wide(&format!("D:P(A;;GA;;;{})", current_user_sid()?));
        let mut sd = null_mut();
        // SAFETY: valid SDDL string; `sd` receives a LocalAlloc'ed descriptor.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let sd = Sd(sd);
        let mut last = None;
        for _ in 0..4 {
            let mut rnd = [0u8; 8];
            getrandom::fill(&mut rnd).map_err(io::Error::other)?;
            let name = format!(r"\\.\pipe\beacon-{key}-{:016x}", u64::from_le_bytes(rnd));
            let w = wide(&name);
            // someone owning the name already makes the first instance fail: next suffix
            match instance(&w, &sd, true) {
                Ok(pending) => {
                    return Ok((
                        Listener {
                            name: w,
                            sd,
                            pending,
                        },
                        name,
                    ));
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap())
    }

    pub(super) fn accept(l: &mut Listener) -> io::Result<Stream> {
        // SAFETY: the pending handle is a server instance we own.
        let r = overlapped(&l.pending, |h, ov| unsafe { ConnectNamedPipe(h, ov) });
        let err = match r {
            Err(e) if e.raw_os_error() != Some(ERROR_PIPE_CONNECTED as i32) => Some(e),
            _ => None,
        };
        // the next instance before handing this one out: the name never disappears
        let next = instance(&l.name, &l.sd, false)?;
        let h = std::mem::replace(&mut l.pending, next);
        match err {
            None => Ok(Stream { h }),
            Some(e) => Err(e),
        }
    }

    pub(super) fn connect(name: &str) -> io::Result<Stream> {
        let w = wide(name);
        loop {
            // SAFETY: valid NUL-terminated name.
            let h = unsafe {
                CreateFileW(
                    w.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    null_mut(),
                )
            };
            match owned(h) {
                Ok(h) => return Ok(Stream { h }),
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    // SAFETY: as above.
                    if unsafe { WaitNamedPipeW(w.as_ptr(), 2000) } == 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Reads back the DACL of the pipe behind `s`: `(is an allow ACE, SID)` per ACE.
    pub fn dacl_aces(s: &Stream) -> io::Result<Vec<(bool, String)>> {
        // SAFETY: out-parameter calls; the descriptor is freed once the ACEs are read.
        unsafe {
            let (mut dacl, mut sd): (*mut ACL, PSECURITY_DESCRIPTOR) = (null_mut(), null_mut());
            let e = GetSecurityInfo(
                s.h.as_raw_handle(),
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut sd,
            );
            if e != 0 {
                return Err(io::Error::from_raw_os_error(e as i32));
            }
            let sd = Sd(sd);
            let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
            if GetAclInformation(
                dacl,
                (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let mut out = vec![];
            for i in 0..info.AceCount {
                let mut ace = null_mut();
                if GetAce(dacl, i, &mut ace) == 0 {
                    return Err(io::Error::last_os_error());
                }
                let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
                // ACCESS_ALLOWED_ACE_TYPE = 0
                let allow = (*ace).Header.AceType == 0;
                let sid = sid_string((&raw mut (*ace).SidStart).cast())?;
                out.push((allow, sid));
            }
            drop(sd);
            Ok(out)
        }
    }
}

#[cfg(windows)]
pub use win::current_user_sid;
/// Test support: the pipe's DACL as `(allow, SID)` per ACE.
#[cfg(windows)]
#[doc(hidden)]
pub use win::dacl_aces;
