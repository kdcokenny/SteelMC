use std::{env::temp_dir, fs, io::ErrorKind, path::PathBuf, ptr};

use uuid::Uuid;

use steel_registry::{init_vanilla_registry, steel_ticket_types};

use super::*;

fn init_registry() {
    let _ = init_vanilla_registry();
}

#[test]
fn duplicate_add_refreshes_timeout_without_adding_multiplicity() {
    let mut storage = ChunkTicketStorage::new();
    let pos = ChunkPos::new(2, -3);
    let ticket = portal_ticket();

    let first = storage.add_ticket(pos, ticket);
    let stale_expirations = storage.timed_ticket_expirations();
    let _ = storage.tick_timed_tickets(&stale_expirations);
    let duplicate = storage.add_ticket(pos, ticket);

    assert_eq!(first.load_positions, vec![pos]);
    assert_eq!(first.simulation_positions, vec![pos]);
    assert_eq!(duplicate, SourceProjectionChanges::default());
    assert_eq!(storage.ticket_count(), 1);
    assert_eq!(storage.timed_ticket_expirations().len(), 1);

    let _ = storage.tick_timed_tickets(&stale_expirations);
    assert_eq!(
        storage.tickets[&pos][0].ticket.ticks_left(),
        vanilla_ticket_types::PORTAL.timeout()
    );

    let removal = storage.remove_ticket(pos, ticket);
    assert_eq!(removal.load_positions, vec![pos]);
    assert_eq!(removal.simulation_positions, vec![pos]);
    assert_eq!(storage.ticket_count(), 0);
}

#[test]
fn type_flags_control_projections_persistence_and_expiration() {
    init_registry();
    let mut storage = ChunkTicketStorage::new();
    let load_pos = ChunkPos::new(0, 0);
    let simulation_pos = ChunkPos::new(1, 0);
    let unknown_pos = ChunkPos::new(2, 0);
    let level = ChunkTicketLevel::BLOCK_TICKING_CHUNK;

    let loading = ChunkTicket::new(&vanilla_ticket_types::PLAYER_LOADING, level);
    let simulation = ChunkTicket::new(&vanilla_ticket_types::PLAYER_SIMULATION, level);
    let forced = ChunkTicket::new(&vanilla_ticket_types::FORCED, level);
    let unknown = ChunkTicket::new(&vanilla_ticket_types::UNKNOWN, level);

    assert_eq!(
        storage.add_ticket(load_pos, loading).simulation_positions,
        Vec::new()
    );
    assert_eq!(storage.load_source_level(load_pos), Some(level));
    assert_eq!(storage.simulation_source_level(load_pos), None);

    assert_eq!(
        storage
            .add_ticket(simulation_pos, simulation)
            .load_positions,
        Vec::new()
    );
    assert_eq!(storage.load_source_level(simulation_pos), None);
    assert_eq!(storage.simulation_source_level(simulation_pos), Some(level));

    let _ = storage.add_ticket(load_pos, forced);
    let _ = storage.add_ticket(unknown_pos, unknown);
    assert_eq!(storage.to_persistent().tickets.len(), 1);

    let expirations = storage.timed_ticket_expirations();
    let unknown_expiration = expirations
        .iter()
        .find(|expiration| expiration.pos() == unknown_pos)
        .copied()
        .expect("unknown ticket should be timed");
    assert!(unknown_expiration.can_expire_if_unloaded());

    let portal_pos = ChunkPos::new(3, 0);
    let _ = storage.add_or_refresh_portal_ticket(portal_pos);
    let portal_expiration = storage
        .timed_ticket_expirations()
        .into_iter()
        .find(|expiration| expiration.pos() == portal_pos)
        .expect("portal ticket should be timed");
    assert!(!portal_expiration.can_expire_if_unloaded());

    let pearl_pos = ChunkPos::new(4, 0);
    let _ = storage.add_or_refresh_ender_pearl_ticket(pearl_pos);
    let pearl_expiration = storage
        .timed_ticket_expirations()
        .into_iter()
        .find(|expiration| expiration.pos() == pearl_pos)
        .expect("ender pearl ticket should be timed");
    assert!(!pearl_expiration.can_expire_if_unloaded());
}

#[test]
fn persistence_resolves_registered_type_and_rejects_invalid_values() {
    init_registry();
    let pos = ChunkPos::new(-8, 12);
    let level = ChunkTicketLevel::BLOCK_TICKING_CHUNK;
    let mut storage = ChunkTicketStorage::new();
    let portal = ChunkTicket::from_saved(&vanilla_ticket_types::PORTAL, level, 123);
    let forced = ChunkTicket::new(&vanilla_ticket_types::FORCED, level);
    let internal = ChunkTicket::new(&steel_ticket_types::CHUNK_REQUEST, level);
    let _ = storage.add_ticket(pos, portal);
    let _ = storage.add_ticket(pos, forced);
    let _ = storage.add_ticket(pos, internal);

    let persistent = storage.to_persistent();
    assert_eq!(persistent.tickets.len(), 2);
    let restored = restore(&persistent);
    assert_eq!(restored.ticket_count(), 2);
    let restored_tickets = &restored.tickets[&pos];
    let forced_ticks_left = restored_tickets
        .iter()
        .find(|stored| {
            ptr::eq(
                stored.ticket.ticket_type(),
                &raw const vanilla_ticket_types::FORCED,
            )
        })
        .map(|stored| stored.ticket.ticks_left());
    let portal_ticks_left = restored_tickets
        .iter()
        .find(|stored| {
            ptr::eq(
                stored.ticket.ticket_type(),
                &raw const vanilla_ticket_types::PORTAL,
            )
        })
        .map(|stored| stored.ticket.ticks_left());
    assert_eq!(forced_ticks_left, Some(0));
    assert_eq!(portal_ticks_left, Some(123));

    let unknown = PersistentChunkTickets {
        tickets: vec![PersistentChunkTicket {
            ticket_type: Identifier::new_static("test", "missing"),
            chunk_x: 0,
            chunk_z: 0,
            level: level.raw(),
            ticks_left: 0,
        }],
    };
    assert_eq!(restore(&unknown).ticket_count(), 0);

    let invalid_level = ChunkTicketLevel::MAX.raw() + 1;
    let invalid = PersistentChunkTickets {
        tickets: vec![PersistentChunkTicket {
            ticket_type: Identifier::vanilla_static("forced"),
            chunk_x: 0,
            chunk_z: 0,
            level: invalid_level,
            ticks_left: 0,
        }],
    };
    assert_eq!(restore(&invalid).ticket_count(), 0);
}

#[test]
fn persistence_defaults_ticks_and_duplicate_activation_refreshes_timeout() {
    init_registry();
    let pos = ChunkPos::new(2, 3);
    let level = ChunkTicketLevel::FULL_CHUNK;
    let encoded = format!(
        "tickets = [{{ type = \"minecraft:portal\", chunk_x = 2, chunk_z = 3, level = {} }}]",
        level.raw()
    );
    let mut persistent: PersistentChunkTickets =
        toml::from_str(&encoded).expect("ticket data without ticks_left should decode");
    assert_eq!(persistent.tickets[0].ticks_left, 0);

    persistent.tickets.push(PersistentChunkTicket {
        ticket_type: Identifier::vanilla_static("portal"),
        chunk_x: pos.0.x,
        chunk_z: pos.0.y,
        level: level.raw(),
        ticks_left: 20,
    });
    let restored = restore(&persistent);

    assert_eq!(restored.ticket_count(), 1);
    assert_eq!(
        restored.tickets[&pos][0].ticket.ticks_left(),
        vanilla_ticket_types::PORTAL.timeout()
    );
}

#[test]
fn timed_decrement_wraps_like_java_long() {
    init_registry();
    let pos = ChunkPos::new(4, 5);
    let persistent = PersistentChunkTickets {
        tickets: vec![PersistentChunkTicket {
            ticket_type: Identifier::vanilla_static("portal"),
            chunk_x: pos.0.x,
            chunk_z: pos.0.y,
            level: ChunkTicketLevel::FULL_CHUNK.raw(),
            ticks_left: i64::MIN,
        }],
    };
    let mut storage = restore(&persistent);

    let expirations = storage.timed_ticket_expirations();
    let _ = storage.tick_timed_tickets(&expirations);

    assert_eq!(storage.tickets[&pos][0].ticket.ticks_left(), i64::MAX);
}

fn decode(encoded: &str) -> ChunkTicketStorage {
    init_registry();
    let persistent = toml::from_str(encoded).expect("test data should be valid TOML");
    ChunkTicketStorage::from_persistent(
        persistent,
        &Identifier::new_static("test", "ticket_recovery"),
    )
}

fn restore(persistent: &PersistentChunkTickets) -> ChunkTicketStorage {
    let encoded = toml::to_string(persistent).expect("ticket data should encode");
    decode(&encoded)
}

fn surrounding_valid_tickets(invalid: &str) -> String {
    format!(
        r#"tickets = [
            {{ type = "minecraft:portal", chunk_x = -8, chunk_z = 12, level = {}, ticks_left = 123 }},
            {invalid},
            {{ type = "minecraft:forced", chunk_x = 20, chunk_z = -7, level = {}, ticks_left = 0 }}
        ]"#,
        ChunkTicketLevel::BLOCK_TICKING_CHUNK.raw(),
        ChunkTicketLevel::FULL_CHUNK.raw(),
    )
}

fn assert_surrounding_tickets_survive(storage: &ChunkTicketStorage) {
    let portal_pos = ChunkPos::new(-8, 12);
    let forced_pos = ChunkPos::new(20, -7);
    assert_eq!(storage.ticket_count(), 2);
    assert_eq!(
        storage.load_source_level(portal_pos),
        Some(ChunkTicketLevel::BLOCK_TICKING_CHUNK)
    );
    assert_eq!(
        storage.simulation_source_level(portal_pos),
        Some(ChunkTicketLevel::BLOCK_TICKING_CHUNK)
    );
    assert_eq!(storage.tickets[&portal_pos][0].ticket.ticks_left(), 123);
    assert_eq!(
        storage.load_source_level(forced_pos),
        Some(ChunkTicketLevel::FULL_CHUNK)
    );
    assert_eq!(storage.tickets[&forced_pos][0].ticket.ticks_left(), 0);
    assert_eq!(storage.timed_ticket_expirations().len(), 1);
}

#[test]
fn valid_invalid_valid_keeps_both_valid_tickets() {
    let invalid_entries = [
        "false".to_owned(),
        "{}".to_owned(),
        format!(
            r#"{{ type = "test:missing", chunk_x = 0, chunk_z = 0, level = {} }}"#,
            ChunkTicketLevel::FULL_CHUNK.raw()
        ),
        format!(
            r#"{{ type = "minecraft:forced", chunk_x = 0, chunk_z = 0, level = {} }}"#,
            ChunkTicketLevel::MAX.raw() + 1
        ),
    ];
    for invalid in invalid_entries {
        let encoded = surrounding_valid_tickets(&invalid);
        assert_surrounding_tickets_survive(&decode(&encoded));
    }
}

#[test]
fn every_required_ticket_field_stays_required() {
    let fields = [
        ("type", r#"type = "minecraft:portal""#.to_owned()),
        ("chunk_x", "chunk_x = 0".to_owned()),
        ("chunk_z", "chunk_z = 0".to_owned()),
        (
            "level",
            format!("level = {}", ChunkTicketLevel::FULL_CHUNK.raw()),
        ),
        ("ticks_left", "ticks_left = 50".to_owned()),
    ];
    for missing in ["type", "chunk_x", "chunk_z", "level"] {
        let fields = fields
            .iter()
            .filter(|(name, _)| *name != missing)
            .map(|(_, field)| field.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let encoded = surrounding_valid_tickets(&format!("{{ {fields} }}"));
        assert!(
            toml::from_str::<PersistentChunkTickets>(&encoded).is_err(),
            "{missing} must not acquire a default"
        );
        assert_surrounding_tickets_survive(&decode(&encoded));
    }
}

#[test]
fn invalid_numeric_and_field_types_do_not_discard_other_tickets() {
    for level in ["-1", "256", "1.5", "false", r#""invalid""#] {
        let invalid = format!(
            r#"{{ type = "minecraft:forced", chunk_x = 0, chunk_z = 0, level = {level} }}"#
        );
        assert_surrounding_tickets_survive(&decode(&surrounding_valid_tickets(&invalid)));
    }
    for fields in [
        "type = false, chunk_x = 0, chunk_z = 0",
        r#"type = "minecraft:forced", chunk_x = 2147483648, chunk_z = 0"#,
        r#"type = "minecraft:forced", chunk_x = 0, chunk_z = "invalid""#,
        r#"type = "minecraft:portal", chunk_x = 0, chunk_z = 0, ticks_left = "invalid""#,
    ] {
        let invalid = format!(
            "{{ {fields}, level = {} }}",
            ChunkTicketLevel::FULL_CHUNK.raw()
        );
        assert_surrounding_tickets_survive(&decode(&surrounding_valid_tickets(&invalid)));
    }
}

#[test]
fn array_of_tables_recovers_after_an_invalid_entry() {
    let encoded = format!(
        r#"
        [[tickets]]
        type = "minecraft:portal"
        chunk_x = -8
        chunk_z = 12
        level = {}
        ticks_left = 123

        [[tickets]]
        type = "minecraft:forced"
        chunk_x = 0
        level = {}

        [[tickets]]
        type = "minecraft:forced"
        chunk_x = 20
        chunk_z = -7
        level = {}
        ticks_left = 0
        "#,
        ChunkTicketLevel::BLOCK_TICKING_CHUNK.raw(),
        ChunkTicketLevel::FULL_CHUNK.raw(),
        ChunkTicketLevel::FULL_CHUNK.raw(),
    );
    assert_surrounding_tickets_survive(&decode(&encoded));
}

#[test]
fn empty_and_fully_rejected_lists_produce_empty_storage() {
    for encoded in ["", "tickets = []", "tickets = [false, {}, 42]"] {
        let storage = decode(encoded);
        assert_eq!(storage.ticket_count(), 0);
        assert_eq!(storage.initial_load_sources(), Vec::new());
        assert_eq!(storage.initial_simulation_sources(), Vec::new());
        assert_eq!(storage.timed_ticket_expirations(), Vec::new());
    }
}

#[test]
fn valid_level_boundaries_and_saved_time_are_preserved() {
    for level in [ChunkTicketLevel::STRONGEST, ChunkTicketLevel::MAX] {
        let persistent = PersistentChunkTickets {
            tickets: vec![PersistentChunkTicket {
                ticket_type: Identifier::vanilla_static("portal"),
                chunk_x: i32::MIN,
                chunk_z: i32::MAX,
                level: level.raw(),
                ticks_left: i64::MAX,
            }],
        };
        assert_eq!(restore(&persistent).to_persistent(), persistent);
    }
}

struct TempWorld(PathBuf);

impl TempWorld {
    fn new() -> Self {
        let path = temp_dir().join(format!("steel-ticket-recovery-{}", Uuid::new_v4()));
        fs::create_dir_all(path.join("data")).expect("test data directory should be created");
        Self(path)
    }

    fn ticket_path(&self) -> PathBuf {
        self.0.join("data/chunk_tickets.toml")
    }
}

impl Drop for TempWorld {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn loading_and_saving_recovers_without_rewriting_the_input_on_load() {
    init_registry();
    let directory = TempWorld::new();
    let saved_data = SavedDataManager::new(Some(&directory.0));
    let world = Identifier::new_static("test", "ticket_recovery");
    let encoded = surrounding_valid_tickets("{}");
    fs::write(directory.ticket_path(), &encoded).expect("test file should be written");

    let storage = ChunkTicketStorage::load(&saved_data, &world).await;
    assert_surrounding_tickets_survive(&storage);
    assert_eq!(
        fs::read_to_string(directory.ticket_path()).expect("input should still exist"),
        encoded
    );

    let persistent = storage.to_persistent();
    saved_data
        .save(saved_data_names::CHUNK_TICKETS, &persistent)
        .await
        .expect("recovered tickets should save");
    let round_trip: PersistentChunkTickets = saved_data
        .load_or_default(saved_data_names::CHUNK_TICKETS)
        .await
        .expect("saved tickets should decode without recovery");
    assert_eq!(round_trip, persistent);
    let reloaded = ChunkTicketStorage::load(&saved_data, &world).await;
    assert_eq!(reloaded.to_persistent(), persistent);
    assert_surrounding_tickets_survive(&reloaded);
}

#[tokio::test]
async fn unusable_ticket_files_load_empty_storage() {
    let directory = TempWorld::new();
    let saved_data = SavedDataManager::new(Some(&directory.0));
    let world = Identifier::new_static("test", "unusable_tickets");
    let unusable: &[&[u8]] = &[
        b"[[tickets]\n",
        b"tickets = 42\n",
        b"tickets = {}\n",
        b"tickets = \"invalid\"\n",
        b"\xff\xfe",
    ];
    for &bytes in unusable {
        fs::write(directory.ticket_path(), bytes).expect("test file should be written");
        let storage = ChunkTicketStorage::load(&saved_data, &world).await;
        assert_eq!(storage.ticket_count(), 0);
        assert_eq!(storage.initial_load_sources(), Vec::new());
        assert_eq!(storage.initial_simulation_sources(), Vec::new());
        assert_eq!(
            fs::read(directory.ticket_path()).expect("unusable file should be left intact"),
            bytes
        );
    }

    let malformed = format!("{}\ninvalid = [", surrounding_valid_tickets("{}"));
    fs::write(directory.ticket_path(), &malformed).expect("test file should be written");
    assert_eq!(
        ChunkTicketStorage::load(&saved_data, &world)
            .await
            .ticket_count(),
        0
    );
}

#[tokio::test]
async fn ticket_file_read_errors_load_empty_storage() {
    let directory = TempWorld::new();
    fs::create_dir(directory.ticket_path()).expect("ticket path should be a directory");
    let storage = ChunkTicketStorage::load(
        &SavedDataManager::new(Some(&directory.0)),
        &Identifier::new_static("test", "unreadable_tickets"),
    )
    .await;
    assert_eq!(storage.ticket_count(), 0);
    assert!(directory.ticket_path().is_dir());
}

#[tokio::test]
async fn missing_and_ephemeral_ticket_storage_is_empty() {
    let directory = TempWorld::new();
    for saved_data in [
        SavedDataManager::new(Some(&directory.0)),
        SavedDataManager::new(None),
    ] {
        let storage = ChunkTicketStorage::load(
            &saved_data,
            &Identifier::new_static("test", "missing_tickets"),
        )
        .await;
        assert_eq!(storage.ticket_count(), 0);
    }
    assert!(!directory.ticket_path().exists());
}

#[tokio::test]
async fn unrelated_saved_data_errors_are_not_suppressed() {
    let directory = TempWorld::new();
    let saved_data = SavedDataManager::new(Some(&directory.0));
    fs::write(directory.0.join("data/scoreboard.toml"), "invalid = [")
        .expect("test file should be written");
    let error = saved_data
        .load_or_default::<toml::Table>(saved_data_names::SCOREBOARD)
        .await
        .expect_err("other saved data must retain strict error handling");
    assert_eq!(error.kind(), ErrorKind::InvalidData);
}
