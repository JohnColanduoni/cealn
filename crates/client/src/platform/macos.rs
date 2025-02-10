use std::io;

pub(crate) type Pid = i32;

#[derive(Debug)]
pub(crate) struct Process {
    pid: Pid,
}

impl Process {
    pub(crate) fn get(pid: Pid) -> anyhow::Result<Option<Process>> {
        unsafe {
            if libc::kill(pid, 0) < 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::ESRCH) {
                    return Ok(None);
                } else {
                    return Err(err.into());
                }
            } else {
                Ok(Some(Process { pid }))
            }
        }
    }

    pub(crate) fn running(&self) -> anyhow::Result<bool> {
        unsafe {
            if libc::kill(self.pid, 0) < 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::ESRCH) {
                    return Ok(false);
                } else {
                    return Err(err.into());
                }
            } else {
                Ok(true)
            }
        }
    }
}
