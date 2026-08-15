use std::{
    env::temp_dir,
    path::{Path, PathBuf},
    slice,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rayon::ThreadPoolBuilder;
use reqwest::Client;
use tokio::{
    fs,
    runtime::Builder,
    spawn,
    sync::mpsc,
    task::{JoinSet, yield_now},
    time::timeout,
};
use uuid::Uuid;

use crate::config::{ResolvedDomainConfig, StorageSelection};
use crate::permission::{PermissionGroupManager, PermissionGroupsConfig, PermissionSubjectIndex};
use crate::player::connection::{
    JavaConnection, JavaNetworkWriter, NetworkConnection as _, OutboundPacket,
};
use crate::player::{
    ClientInformation, GameProfile, KnownPlayers, Player, PlayerConnection, ResetReason,
};
use crate::test_support::{fresh_test_world, test_runtime_config};
use crate::world::World;

use super::super::{
    AsyncMutex, CancellationToken, CommandRegistry, CommandRequestQueue, DomainCommandStorage,
    DomainScoreboards, FxHashMap, KeyStore, Notify, PacketProcessor, PlayerDataStorage, PlayerMap,
    RegistryCache, Server, ServerJobQueue, ServiceKeyStore, SyncMutex, SyncRwLock, WorldMap,
    create_registered_dispatcher,
};
use super::{PlayerAdmissionState, PlayerDisconnectQueue, PlayerJoinQueue};

fn test_storage_root(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    temp_dir().join(format!("steel-server-{name}-{unique}"))
}

async fn test_server(world: Arc<World>, storage_root: &Path) -> Result<Arc<Server>, String> {
    let domain = ResolvedDomainConfig {
        name: world.domain().to_owned(),
        default_world: world.key.clone(),
        worlds: vec![world.key.clone()],
    };
    let mut worlds = WorldMap::new(domain.name.clone(), slice::from_ref(&domain), &[]);
    worlds.insert(world.key.clone(), Arc::clone(&world));

    let scoreboards = DomainScoreboards::load(&worlds)
        .await
        .map_err(|error| format!("test scoreboards should load: {error}"))?;
    let command_storage = DomainCommandStorage::load(&worlds)
        .await
        .map_err(|error| format!("test command storage should load: {error}"))?;
    let player_data_storage = PlayerDataStorage::new(
        storage_root.to_owned(),
        StorageSelection::default_player_file(),
    )
    .await
    .map_err(|error| format!("test player storage should initialize: {error}"))?;
    let registered_commands = create_registered_dispatcher(CommandRegistry::new())
        .map_err(|error| format!("test commands should register: {error}"))?;
    let command_permission_keys = registered_commands
        .permissions
        .iter()
        .map(|permission| permission.as_str().to_owned())
        .collect();
    let permission_groups = PermissionGroupManager::transient(PermissionGroupsConfig::default())
        .map_err(|error| format!("test permission groups should resolve: {error}"))?;
    let config = test_runtime_config(1);
    let registry_cache = RegistryCache::new(config.compression);

    Ok(Arc::new(Server {
        config,
        permission_groups,
        cancel_token: CancellationToken::new(),
        key_store: KeyStore::create(),
        registry_cache,
        worlds,
        online_players: PlayerMap::new(),
        player_admissions: SyncMutex::new(FxHashMap::default()),
        tick_rate_manager: SyncRwLock::new(super::super::TickRateManager::new()),
        scoreboards,
        command_storage,
        command_dispatcher: SyncRwLock::new(registered_commands.dispatcher),
        command_permission_keys,
        command_requests: CommandRequestQueue::new(),
        packet_processor: PacketProcessor::new(),
        chunk_encoding_pool: Arc::new(
            ThreadPoolBuilder::new()
                .num_threads(1)
                .build()
                .expect("test chunk encoding pool should initialize"),
        ),
        jobs: ServerJobQueue::new(),
        player_data_storage,
        player_permission_states: SyncRwLock::new(PermissionSubjectIndex::new()),
        player_permission_updates: AsyncMutex::new(()),
        known_players: SyncMutex::new(
            super::super::KnownPlayerCacheState::new(KnownPlayers::new()),
        ),
        known_player_save_idle: Notify::new(),
        profile_lookup_client: Client::new(),
        service_keys: Arc::new(
            ServiceKeyStore::new(None).expect("test services key store should initialize"),
        ),
        pending_player_joins: PlayerJoinQueue::new(),
        pending_player_disconnects: PlayerDisconnectQueue::new(),
        pending_world_changes: SyncMutex::new(Vec::new()),
        pending_domain_switches: SyncMutex::new(Vec::new()),
    }))
}

fn java_test_player(
    server: &Arc<Server>,
    world: Arc<World>,
    uuid: Uuid,
) -> (
    Arc<Player>,
    mpsc::UnboundedReceiver<OutboundPacket>,
    JavaNetworkWriter,
) {
    let (outgoing_packets, receiver) = mpsc::unbounded_channel();
    let cancel_token = CancellationToken::new();
    let network_writer: JavaNetworkWriter = Arc::new(AsyncMutex::new(None));
    let player = Arc::new_cyclic(|player_weak| {
        let connection = Arc::new(PlayerConnection::Java(JavaConnection::new(
            outgoing_packets.clone(),
            cancel_token.clone(),
            None,
            Arc::clone(&network_writer),
            1,
            player_weak.clone(),
        )));
        Player::new(
            GameProfile {
                id: uuid,
                name: "TestPlayer".to_owned(),
                properties: Vec::new(),
                profile_actions: None,
            },
            connection,
            world,
            Arc::downgrade(server),
            Arc::clone(&server.config),
            1,
            ClientInformation::default(),
        )
    });
    (player, receiver, network_writer)
}

#[test]
fn disconnect_cleanup_does_not_wait_for_final_socket_write() {
    let world = fresh_test_world("disconnect_blocked_writer");
    let runtime = Builder::new_current_thread().enable_all().build();
    let Ok(runtime) = runtime else {
        panic!("test runtime should initialize");
    };

    runtime.block_on(async {
        let storage_root = test_storage_root("disconnect-blocked-writer");
        let server = test_server(Arc::clone(&world), &storage_root).await;
        let Ok(server) = server else {
            panic!("test server should initialize");
        };
        let (player, receiver, network_writer) =
            java_test_player(&server, Arc::clone(&world), Uuid::from_u128(1));

        assert!(server.online_players.insert(Arc::clone(&player)));
        assert!(world.add_player(Arc::clone(&player), ResetReason::InitialJoin));
        let _ = player.mark_joined_world();

        let writer_guard = network_writer.lock().await;
        let sender_player = Arc::clone(&player);
        let sender_task = spawn(async move {
            let PlayerConnection::Java(connection) = &*sender_player.connection else {
                panic!("test player should use a Java connection");
            };
            connection.sender(receiver).await;
        });

        player.disconnect("test disconnect");
        let pending = timeout(Duration::from_secs(1), async {
            loop {
                let pending = server.process_player_disconnects();
                if !pending.is_empty() {
                    break pending;
                }
                yield_now().await;
            }
        })
        .await;
        let Ok(pending) = pending else {
            panic!("authoritative cleanup waited for the blocked disconnect write");
        };

        assert_eq!(pending.len(), 1);
        assert!(
            server
                .online_players
                .get_by_uuid(&player.gameprofile.id)
                .is_none()
        );
        assert!(world.players.get_by_uuid(&player.gameprofile.id).is_none());

        drop(pending);
        drop(writer_guard);
        match timeout(Duration::from_secs(3), sender_task).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => panic!("sender task failed: {error}"),
            Err(error) => {
                panic!("sender task did not stop after the writer was released: {error}")
            }
        }

        drop(player);
        drop(network_writer);
        drop(server);
        if let Err(error) = fs::remove_dir_all(&storage_root).await {
            panic!("test storage should be removed: {error}");
        }
    });
}

#[test]
fn duplicate_login_evicts_existing_player_and_waits_for_persistence() {
    let world = fresh_test_world("duplicate_login_wait");
    let runtime = Builder::new_current_thread().enable_all().build();
    let Ok(runtime) = runtime else {
        panic!("test runtime should initialize");
    };

    runtime.block_on(async {
        let storage_root = test_storage_root("duplicate-login-wait");
        let server = test_server(Arc::clone(&world), &storage_root).await;
        let Ok(server) = server else {
            panic!("test server should initialize");
        };
        let uuid = Uuid::from_u128(1);
        let (player, _receiver, _network_writer) =
            java_test_player(&server, Arc::clone(&world), uuid);

        assert!(server.online_players.insert(Arc::clone(&player)));
        assert!(world.add_player(Arc::clone(&player), ResetReason::InitialJoin));
        let _ = player.mark_joined_world();

        let reservation_server = Arc::clone(&server);
        let reservation_task = spawn(async move {
            let cancellation = CancellationToken::new();
            reservation_server
                .reserve_replacement_player_join(uuid, &cancellation)
                .await
        });

        if timeout(Duration::from_secs(1), async {
            while !player.connection.closed() {
                yield_now().await;
            }
        })
        .await
        .is_err()
        {
            panic!("duplicate login did not evict the existing session");
        }
        assert!(
            !reservation_task.is_finished(),
            "replacement must wait until the old player is detached and saved"
        );

        let mut disconnect_saves = JoinSet::new();
        server.start_player_disconnect_saves(&mut disconnect_saves);
        let reservation = match timeout(Duration::from_secs(5), reservation_task).await {
            Ok(Ok(Ok(reservation))) => reservation,
            Ok(Ok(Err(error))) => panic!("replacement reservation failed: {error:?}"),
            Ok(Err(error)) => panic!("replacement reservation task failed: {error}"),
            Err(error) => {
                panic!("replacement reservation did not observe completed removal: {error}")
            }
        };

        assert!(server.online_players.get_by_uuid(&uuid).is_none());
        assert_eq!(
            server.player_admissions.lock().get(&uuid),
            Some(&PlayerAdmissionState::Joining)
        );
        while let Some(result) = disconnect_saves.join_next().await {
            if let Err(error) = result {
                panic!("disconnect save task failed: {error}");
            }
        }

        drop(reservation);
        assert!(server.player_admissions.lock().get(&uuid).is_none());

        drop(player);
        drop(server);
        if let Err(error) = fs::remove_dir_all(&storage_root).await {
            panic!("test storage should be removed: {error}");
        }
    });
}
