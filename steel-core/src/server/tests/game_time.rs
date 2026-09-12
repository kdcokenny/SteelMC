use super::*;
use crate::server::world_tick_workers::WorldTickWorkers;
use crate::test_support::test_domain;
use steel_registry::{packets::play::C_SET_TIME, vanilla_world_clocks};

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one continuous timeline verifies transfer expiry through freeze, steps, and sprint"
)]
fn game_time_domains_freeze_steps_sprint_and_transfer_damage() {
    let first = test_domain("clock_a", &["primary", "derived", "third"]);
    let second = test_domain("clock_b", &["primary"]);
    for _ in 0..100 {
        second.advance_domain_game_times();
    }
    let primary = Arc::clone(first.default_world("clock_a").expect("primary"));
    let derived = Arc::clone(
        first
            .get(&steel_utils::Identifier::new_static("clock_a", "derived"))
            .expect("derived"),
    );
    let domains: Vec<_> = [&first, &second]
        .iter()
        .map(|map| ResolvedDomainConfig {
            name: map.default_domain().to_owned(),
            default_world: map.server_default_world().expect("primary").key.clone(),
            worlds: map.keys().cloned().collect(),
        })
        .collect();
    let loaded: Vec<_> = first.values().chain(second.values()).cloned().collect();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let root = test_storage_root("game-time-domain-ticks");
        let server = test_server_with_worlds(
            "clock_a".to_owned(),
            &domains,
            &loaded,
            PermissionSubjectIndex::new(),
            &root,
        )
        .await
        .expect("server");
        server.worlds.validate_game_times().expect("bound domains");
        let workers = WorldTickWorkers::spawn(server.worlds.values()).expect("workers");
        let player = test_player(&server, Arc::clone(&primary));
        let source = DamageSource::environment(&vanilla_damage_types::GENERIC);
        player
            .living_base()
            .record_last_damage_source(&source, primary.game_time());
        for tick in 1..=39 {
            server
                .tick_worlds_game(&workers, tick, true)
                .await
                .expect("tick");
        }
        assert!(player.last_damage_source().is_some());
        player.reset(Arc::clone(&derived), ResetReason::WorldChange);
        assert!(
            player.last_damage_source().is_some(),
            "same-domain transfer preserves history"
        );
        server.tick_rate_manager.write().set_frozen(true);
        for tick in 40..50 {
            let runs = next_simulation_gate(&server);
            server
                .tick_worlds_game(&workers, tick, runs)
                .await
                .expect("frozen tick");
        }
        assert_eq!(primary.game_time(), 39);
        assert!(player.last_damage_source().is_some());
        assert!(server.tick_rate_manager.write().step_game_if_paused(2));
        for (tick, available) in [(50, true), (51, false)] {
            let runs = next_simulation_gate(&server);
            server
                .tick_worlds_game(&workers, tick, runs)
                .await
                .expect("step");
            assert_eq!(player.last_damage_source().is_some(), available);
        }
        assert_eq!(derived.game_time(), 41);
        server.tick_rate_manager.write().request_game_to_sprint(3);
        for tick in 52..55 {
            let runs = {
                let mut manager = server.tick_rate_manager.write();
                manager.tick();
                assert!(manager.check_should_sprint_this_tick().0);
                manager.runs_normally()
            };
            server
                .tick_worlds_game(&workers, tick, runs)
                .await
                .expect("sprint");
            server.tick_rate_manager.write().end_tick_work();
        }
        for world in first.values() {
            assert_eq!(world.game_time(), 44);
        }
        assert_eq!(
            second
                .server_default_world()
                .expect("second primary")
                .game_time(),
            144
        );
        drop(workers);
        server.cancel_token.cancel();
        fs::remove_dir_all(root).await.expect("cleanup");
    });
}

#[test]
fn game_time_full_partial_periodic_packets_keep_world_clocks_independent() {
    let worlds = test_domain("packets", &["primary", "derived"]);
    let primary = worlds.default_world("packets").expect("primary");
    let derived = worlds
        .get(&steel_utils::Identifier::new_static("packets", "derived"))
        .expect("derived");
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let root = test_storage_root("game-time-packets");
        let server = test_server(Arc::clone(primary), PermissionSubjectIndex::new(), &root)
            .await
            .expect("server");
        let (player, packets) =
            test_player_with_packets(&server, Arc::clone(derived), "ClockTest", 771_234);
        assert!(derived.add_player(Arc::clone(&player), ResetReason::InitialJoin));
        packets.lock().clear();
        for _ in 0..20 {
            worlds.advance_domain_game_times();
        }
        derived
            .set_clock_total_ticks(&vanilla_world_clocks::OVERWORLD, 6000)
            .expect("clock");
        derived
            .set_clock_rate(&vanilla_world_clocks::OVERWORLD, 2.0)
            .expect("rate");
        derived
            .set_clock_paused(&vanilla_world_clocks::THE_END, true)
            .expect("pause");
        derived.broadcast_time_sync();
        derived.tick_game(20, true);
        let times: Vec<_> = packets
            .lock()
            .iter()
            .filter(|packet| packet_id(packet) == C_SET_TIME)
            .map(|packet| {
                let mut cursor = Cursor::new(packet.encoded_data.as_slice());
                VarInt::read(&mut cursor).expect("length");
                VarInt::read(&mut cursor).expect("id");
                (
                    i64::read(&mut cursor).expect("game time"),
                    VarInt::read(&mut cursor).expect("clock count").0,
                )
            })
            .collect();
        assert_eq!(times, vec![(20, 1), (20, 1), (20, 1), (20, 2), (20, 0)]);
        assert_eq!(primary.game_time(), 20);
        assert_eq!(derived.time_sync_packet().game_time, 20);
        assert_eq!(
            primary.clock_total_ticks(&vanilla_world_clocks::OVERWORLD),
            Some(0)
        );
        assert_eq!(
            derived.clock_total_ticks(&vanilla_world_clocks::OVERWORLD),
            Some(6002)
        );
        assert_eq!(
            derived.clock_total_ticks(&vanilla_world_clocks::THE_END),
            Some(0)
        );
        server.cancel_token.cancel();
        fs::remove_dir_all(root).await.expect("cleanup");
    });
}

#[test]
fn game_time_startup_uses_configured_primary_even_when_listed_last() {
    with_server_runtime(|runtime| {
        runtime.block_on(async {
            let root = test_storage_root("game-time-primary-last");
            let config_text = format!(
                r#"
save_path = '{}'
[domains.custom]
default = true
[[domains.custom.worlds]]
name = "derived"
generator = "minecraft:flat"
[[domains.custom.worlds]]
name = "authority"
generator = "minecraft:flat"
default = true
[domains.custom.worlds.config]
dimension_type = "minecraft:the_end"
"#,
                root.display()
            );
            let config = || {
                let mut config = RuntimeConfig::clone(&test_runtime_config());
                config.services_server = Some(UNROUTABLE_SERVICES.to_owned());
                config
            };
            let start = async || {
                Server::new(
                    Arc::clone(runtime),
                    CancellationToken::new(),
                    config(),
                    toml::from_str(&config_text).expect("world config"),
                    PermissionGroupManager::transient(PermissionGroupsConfig::default())
                        .expect("permissions"),
                )
                .await
                .expect("startup")
            };
            let server = start().await;
            let primary = server.worlds.default_world("custom").expect("primary");
            assert_eq!(primary.key.path.as_ref(), "authority");
            for _ in 0..73 {
                server.worlds.advance_domain_game_times();
            }
            let mut saved_chunks = 0;
            for world in server.worlds.values() {
                world.cleanup(&mut saved_chunks).await;
            }
            server.cancel_token.cancel();
            drop(server);
            let restarted = start().await;
            for world in restarted.worlds.values() {
                assert_eq!(world.game_time(), 73);
            }
            restarted
                .worlds
                .validate_game_times()
                .expect("restart binding");
            restarted.cancel_token.cancel();
            drop(restarted);
            fs::remove_dir_all(root).await.expect("cleanup");
        });
    });
}

fn next_simulation_gate(server: &Server) -> bool {
    let mut manager = server.tick_rate_manager.write();
    manager.tick();
    manager.runs_normally()
}
