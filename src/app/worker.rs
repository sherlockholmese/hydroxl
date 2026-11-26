//! Hosts blocking work. The worker receives owned inputs and has no access to UI state.
use super::{
    Job, JobEvent,
    jobs::{LibraryUpdate, REFRESH_BUDGET},
};
use crate::{
    appliance,
    backend::{self, Action, Connection, Machine, State},
    operations::{self, Operation},
};
use std::{sync::mpsc, time::Instant};

pub(super) fn run(
    job: Job,
    c: Connection,
    selected: Option<String>,
    mut previous: Vec<Machine>,
    need_architectures: bool,
    progress: backend::progress::Reporter,
    tx: mpsc::Sender<JobEvent>,
) {
    let refresh = matches!(job, Job::Refresh);
    let importing = matches!(&job, Job::Import(..) | Job::Appliance(_));
    let label = job.label();
    let mut select_name = match &job {
        Job::Create(s) => Some(s.name.trim().to_owned()),
        Job::Operation(
            _,
            Operation::Clone(name) | Operation::CloneConfiguration(name) | Operation::Rename(name),
        ) => Some(name.trim().to_owned()),
        Job::Operation(m, Operation::Disposable) => Some(format!("{} Disposable", m.name)),
        _ => None,
    };
    let open_display = match &job {
        Job::Action(machine, Action::Start | Action::Resume, source) => {
            let mut machine = machine.clone();
            machine.state = State::Running;
            machine.saved_state = false;
            Some((machine, *source))
        }
        _ => None,
    };
    let disk_conversion = matches!(job, Job::ConvertDisk { .. });
    let outcome = match job {
        Job::Refresh | Job::Icon(..) => Ok(()),
        Job::Network(net) => backend::network::create(c, &net),
        Job::Action(m, action, _) => backend::action(c, &m.id, action, m.spice_viewer),
        Job::Create(spec) => backend::create(c, &spec, &progress),
        Job::Import(path, qemu_build, hardware) => backend::import(c, &path, qemu_build, hardware),
        Job::ConvertDisk {
            source,
            destination,
            format,
        } => backend::disk_convert::convert(&source, &destination, &format, &progress),
        Job::Appliance(spec) => appliance::import(c, &spec, &progress),
        Job::Operation(m, op) => operations::execute(c, &m, op, &progress),
    }
    .map_err(|error| {
        eprintln!("{label}: {error:#}");
        format!("{error:#}")
    });
    let succeeded = outcome.is_ok();
    if !refresh {
        let display = open_display.filter(|_| succeeded).map(|(machine, source)| {
            if let Some(cached) = previous.iter_mut().find(|cached| cached.id == machine.id) {
                cached.state = machine.state;
                cached.saved_state = false;
            }
            Box::new((machine, source, Instant::now()))
        });
        if tx
            .send(JobEvent::OperationFinished {
                outcome,
                display,
            })
            .is_err()
        {
            return;
        }
    }
    if disk_conversion {
        let _ = tx.send(JobEvent::RefreshFinished(None));
        return;
    }

    let sampled_at = Instant::now();
    let deadline = sampled_at + REFRESH_BUDGET;
    backend::with_command_deadline(deadline, || {
        let machines = backend::machines(c, &previous).map_err(|error| format!("{error:#}"));
        let list = machines.as_ref().ok().map(|list| list.machines.as_slice()).unwrap_or(&[]);
        if !succeeded {
            select_name = None;
        } else if importing {
            select_name = list
                .iter()
                .find(|m| !previous.iter().any(|cached| cached.id == m.id))
                .map(|m| m.name.clone());
        }
        let details_for = select_name
            .as_ref()
            .and_then(|name| list.iter().find(|m| &m.name == name))
            .or_else(|| selected.as_ref().and_then(|id| list.iter().find(|m| &m.id == id)))
            .or_else(|| list.first())
            .map(|machine| machine.id.clone());
        let loaded = machines.is_ok();
        if tx
            .send(JobEvent::Library(Box::new(LibraryUpdate {
                sampled_at,
                machines,
                select_name,
                details_for: details_for.clone(),
            })))
            .is_err()
        {
            return;
        }
        if let Some(id) = details_for {
            let details = operations::details(c, &id).map_err(|error| {
                eprintln!("Could not load details for {id}: {error:#}");
                format!("{error:#}")
            });
            if tx.send(JobEvent::Details(id, Box::new(details))).is_err() {
                return;
            }
        }
        let architectures =
            (loaded && need_architectures).then(|| operations::available_architectures(c));
        let _ = tx.send(JobEvent::RefreshFinished(architectures));
    });
}
