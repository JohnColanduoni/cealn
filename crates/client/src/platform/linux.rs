use procfs::{process::ProcState, ProcError};

pub(crate) type Pid = i32;

#[derive(Debug)]
pub(crate) struct Process {
    process: procfs::process::Process,
}

impl Process {
    pub(crate) fn get(pid: Pid) -> anyhow::Result<Option<Process>> {
        match procfs::process::Process::new(pid) {
            Ok(process) => Ok(Some(Process { process })),
            Err(ProcError::NotFound(_)) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    pub(crate) fn running(&self) -> anyhow::Result<bool> {
        let stat = self.process.stat()?;
        Ok(match stat.state()? {
            ProcState::Zombie => false,
            ProcState::Stopped => false,
            ProcState::Dead => false,
            _ => true,
        })
    }
}
