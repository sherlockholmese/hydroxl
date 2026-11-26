//! Hosts blocking work. The worker receives owned inputs and has no access to UI state.
use super::{Job, JobEvent, Response};
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
    previous: Vec<Machine>,
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
            Some((machine, *source))
        }
        _ => None,
    };
    let disk_conversion = matches!(job, Job::ConvertDisk { .. });
    let outcome = match job {
        Job::Refresh | Job::Icon(..) => Ok(()),
        Job::Network(net) => backend::network::create(c, &net),
        Job::Action(m, action, _) => backend::action(c, &m.id, action, m.spice_viewer),
        Job::Create(spec) => backend::create(c, &spec),
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
    if outcome.is_ok()
        && let Some((machine, source)) = open_display
    {
        let _ = tx.send(JobEvent::DisplayReady(Box::new(machine), source));
    }
    if outcome.is_ok() && !disk_conversion && !refresh {
        progress.update("Refreshing library", None);
    }
    let machines = (!disk_conversion)
        .then(|| (Instant::now(), backend::machines(c, &previous).map_err(|e| format!("{e:#}"))));
    let list = machines
        .as_ref()
        .and_then(|(_, result)| result.as_ref().ok())
        .map(|list| list.machines.as_slice());
    if outcome.is_err() {
        select_name = None;
    } else if importing {
        select_name = list
            .and_then(|list| list.iter().find(|m| !previous.iter().any(|cached| cached.id == m.id)))
            .map(|m| m.name.clone());
    }
    let details = list
        .and_then(|list| {
            select_name
                .as_ref()
                .and_then(|name| list.iter().find(|m| &m.name == name))
                .or_else(|| selected.as_ref().and_then(|id| list.iter().find(|m| &m.id == id)))
                .or_else(|| list.first())
        })
        .map(|machine| {
            (
                machine.id.clone(),
                operations::details(c, &machine.id).map_err(|error| {
                    eprintln!("Could not load details for {}: {error:#}", machine.name);
                    format!("{error:#}")
                }),
            )
        });
    let _ = tx.send(JobEvent::Complete(Box::new(Response {
        machines,
        details,
        outcome: (!refresh).then_some(outcome),
        select_name,
        architectures: need_architectures.then(|| operations::available_architectures(c)),
    })));
}
