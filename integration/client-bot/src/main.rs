//! Protocol-776 headless join and chunk-exploration probe.

use std::{collections::HashSet, env, sync::Arc, time::Instant};

use azalea::{ClientInformation, prelude::*};
use parking_lot::Mutex;
use serde_json::json;

#[derive(Clone, Component)]
struct State(Arc<Mutex<Run>>);

struct Run {
    started: Instant,
    spawned_micros: Option<u64>,
    ticks: u64,
    received_chunks: u64,
    phase_chunks: Vec<HashSet<(i32, i32)>>,
    phase_centers: Vec<Option<(i32, i32)>>,
    keepalives: u64,
    next_waypoint: usize,
    waypoints: Vec<(i32, i32)>,
    dwell_ticks: u64,
    minimum_chunks_per_phase: usize,
    view_distance: u8,
    finished: bool,
}

impl Default for State {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(Run {
            started: Instant::now(),
            spawned_micros: None,
            ticks: 0,
            received_chunks: 0,
            phase_chunks: vec![HashSet::new()],
            phase_centers: vec![None],
            keepalives: 0,
            next_waypoint: 0,
            waypoints: parse_waypoints(),
            dwell_ticks: parse_env("STEEL_CLIENT_DWELL_TICKS", 100_u64),
            minimum_chunks_per_phase: parse_env("STEEL_CLIENT_MIN_CHUNKS_PER_PHASE", 9_usize),
            view_distance: parse_env("STEEL_CLIENT_VIEW_DISTANCE", 4_u8),
            finished: false,
        })))
    }
}

#[tokio::main]
async fn main() -> AppExit {
    let address = env::var("STEEL_CLIENT_ADDRESS").unwrap_or_else(|_| "127.0.0.1:25565".to_owned());
    let username = env::var("STEEL_CLIENT_USERNAME").unwrap_or_else(|_| "SteelProbe".to_owned());
    let state = State::default();
    if let Err(error) = validate_probe_config(&state.0.lock()) {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "ok": false,
                "error": error,
                "protocol": 776,
            }))
            .unwrap_or_else(|_| r#"{"ok":false,"error":"JSON serialization failed"}"#.to_owned())
        );
        return AppExit::error();
    }
    ClientBuilder::new()
        .set_handler(handle)
        .set_state(state)
        .start(Account::offline(&username), address)
        .await
}

async fn handle(bot: Client, event: Event, state: State) -> eyre::Result<()> {
    match event {
        Event::Init => {
            let view_distance = state.0.lock().view_distance;
            bot.set_client_information(ClientInformation {
                view_distance,
                ..Default::default()
            })?;
        }
        Event::Spawn => {
            let first_spawn = {
                let mut run = state.0.lock();
                let first_spawn = run.spawned_micros.is_none();
                if first_spawn {
                    run.spawned_micros = Some(elapsed_micros(run.started));
                }
                first_spawn
            };
            if first_spawn {
                bot.chat("/gamemode spectator @s");
            }
        }
        Event::ReceiveChunk(position) => {
            let mut run = state.0.lock();
            run.received_chunks += 1;
            let center = run.phase_centers.last().copied().flatten();
            let belongs_to_phase =
                chunk_belongs_to_phase(position.x, position.z, center, run.view_distance);
            if belongs_to_phase && let Some(phase) = run.phase_chunks.last_mut() {
                phase.insert((position.x, position.z));
            }
        }
        Event::KeepAlive(_) => state.0.lock().keepalives += 1,
        Event::Tick => on_tick(&bot, &state),
        Event::ConnectionFailed(error) => {
            finish_error(&bot, &state, format!("connection failed: {error}"));
        }
        Event::Disconnect(reason) => {
            if !state.0.lock().finished {
                finish_error(&bot, &state, format!("unexpected disconnect: {reason:?}"));
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_probe_config(run: &Run) -> Result<(), String> {
    validate_probe_limits(
        run.dwell_ticks,
        run.minimum_chunks_per_phase,
        run.view_distance,
    )
}

fn validate_probe_limits(
    dwell_ticks: u64,
    minimum_chunks_per_phase: usize,
    view_distance: u8,
) -> Result<(), String> {
    if dwell_ticks == 0 {
        return Err("STEEL_CLIENT_DWELL_TICKS must be greater than zero".to_owned());
    }
    if minimum_chunks_per_phase == 0 {
        return Err("STEEL_CLIENT_MIN_CHUNKS_PER_PHASE must be greater than zero".to_owned());
    }
    if !(2..=32).contains(&view_distance) {
        return Err("STEEL_CLIENT_VIEW_DISTANCE must be in 2..=32".to_owned());
    }
    Ok(())
}

fn on_tick(bot: &Client, state: &State) {
    let mut run = state.0.lock();
    if run.finished || run.spawned_micros.is_none() {
        return;
    }
    run.ticks += 1;
    if run.ticks % run.dwell_ticks == 0 && run.next_waypoint < run.waypoints.len() {
        let (x, z) = run.waypoints[run.next_waypoint];
        run.next_waypoint += 1;
        run.phase_chunks.push(HashSet::new());
        run.phase_centers.push(Some((x, z)));
        bot.chat(format!("/teleport @s {x} 128 {z}"));
        return;
    }
    let finish_tick =
        run.dwell_ticks * (u64::try_from(run.waypoints.len()).unwrap_or(u64::MAX) + 1);
    if run.next_waypoint == run.waypoints.len() && run.ticks >= finish_tick {
        let failure = if run.keepalives == 0 {
            Some("no keepalive was observed".to_owned())
        } else {
            run.phase_chunks
                .iter()
                .enumerate()
                .find(|(_, chunks)| chunks.len() < run.minimum_chunks_per_phase)
                .map(|(phase, chunks)| {
                    format!(
                        "phase {phase} received {} unique chunks, expected at least {}",
                        chunks.len(),
                        run.minimum_chunks_per_phase
                    )
                })
        };
        if let Some(failure) = failure {
            emit_result(&run, false, Some(&failure));
        } else {
            emit_result(&run, true, None);
        }
        run.finished = true;
        bot.exit();
    }
}

fn chunk_belongs_to_phase(
    chunk_x: i32,
    chunk_z: i32,
    center: Option<(i32, i32)>,
    view_distance: u8,
) -> bool {
    center.is_none_or(|(block_x, block_z)| {
        let center_x = block_x.div_euclid(16);
        let center_z = block_z.div_euclid(16);
        let radius = u32::from(view_distance) + 2;
        chunk_x.abs_diff(center_x) <= radius && chunk_z.abs_diff(center_z) <= radius
    })
}

fn finish_error(bot: &Client, state: &State, reason: String) {
    let mut run = state.0.lock();
    if run.finished {
        return;
    }
    emit_result(&run, false, Some(&reason));
    run.finished = true;
    bot.exit();
}

fn emit_result(run: &Run, ok: bool, error: Option<&str>) {
    println!(
        "{}",
        serde_json::to_string(&json!({
            "ok": ok,
            "error": error,
            "protocol": 776,
            "elapsed_micros": elapsed_micros(run.started),
            "spawned_micros": run.spawned_micros,
            "ticks": run.ticks,
            "keepalives": run.keepalives,
            "received_chunk_events": run.received_chunks,
            "unique_chunks_by_phase": run.phase_chunks.iter().map(HashSet::len).collect::<Vec<_>>(),
            "phase_centers": run.phase_centers,
            "waypoints": run.waypoints,
            "view_distance": run.view_distance,
        }))
        .unwrap_or_else(|_| r#"{"ok":false,"error":"JSON serialization failed"}"#.to_owned())
    );
}

fn parse_waypoints() -> Vec<(i32, i32)> {
    let value =
        env::var("STEEL_CLIENT_WAYPOINTS").unwrap_or_else(|_| "0,0;512,0;512,512;0,512".to_owned());
    value
        .split(';')
        .map(|waypoint| {
            let Some((x, z)) = waypoint.split_once(',') else {
                panic!("waypoints must use x,z;x,z")
            };
            (
                x.parse()
                    .unwrap_or_else(|_| panic!("waypoint x must be an integer")),
                z.parse()
                    .unwrap_or_else(|_| panic!("waypoint z must be an integer")),
            )
        })
        .collect()
}

fn parse_env<T>(name: &str, default: T) -> T
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    env::var(name).map_or(default, |value| {
        value
            .parse()
            .unwrap_or_else(|error| panic!("invalid {name}: {error}"))
    })
}

fn elapsed_micros(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::{chunk_belongs_to_phase, validate_probe_limits};

    #[test]
    fn zero_probe_limits_cannot_disable_exploration_checks() {
        assert!(validate_probe_limits(0, 9, 4).is_err());
        assert!(validate_probe_limits(100, 0, 4).is_err());
        assert!(validate_probe_limits(100, 9, 1).is_err());
        assert!(validate_probe_limits(100, 9, 4).is_ok());
    }

    #[test]
    fn separated_phase_rejects_late_chunks_from_previous_view() {
        assert!(chunk_belongs_to_phase(32, 32, Some((512, 512)), 4));
        assert!(!chunk_belongs_to_phase(0, 0, Some((512, 512)), 4));
        assert!(chunk_belongs_to_phase(i32::MIN, i32::MAX, None, 4));
    }
}
