use super::tests::{settings, temp_level_data_dir};
use super::*;
use steel_registry::{init_vanilla_registry, vanilla_dimension_types};

async fn load(path: &Path, source: GameTimeSource) -> io::Result<LevelDataManager> {
    LevelDataManager::new(
        Some(path),
        7,
        Difficulty::Normal,
        settings(
            "minecraft:overworld",
            vanilla_dimension_types::OVERWORLD.height,
        ),
        source,
    )
    .await
}

async fn write_time(path: &Path, value: Option<toml::Value>) {
    let mut data = toml::Table::try_from(LevelData::new_with_seed(7)).expect("serialize fixture");
    data.remove("game_time");
    if let Some(value) = value {
        data.insert("game_time".to_owned(), value);
    }
    fs::write(
        path.join("level.toml"),
        toml::to_string(&data).expect("serialize table"),
    )
    .await
    .expect("write fixture");
}

#[tokio::test]
async fn game_time_primary_only_survives_both_shutdown_save_orders() {
    init_vanilla_registry();
    for primary_first in [true, false] {
        let primary_dir = temp_level_data_dir("clock-primary");
        let derived_dir = temp_level_data_dir("clock-derived");
        write_time(&primary_dir, Some(1234.into())).await;
        write_time(&derived_dir, Some(987_654.into())).await;
        let mut primary = load(&primary_dir, GameTimeSource::Primary)
            .await
            .expect("primary");
        let clock = primary.game_time_handle();
        let mut derived = load(&derived_dir, GameTimeSource::Derived(Arc::clone(&clock)))
            .await
            .expect("derived");
        assert!(Arc::ptr_eq(&clock, &derived.game_time_handle()));
        assert_eq!(clock.ticks(), 1234);
        assert!(derived.is_dirty());
        primary.save().await.expect("save initialized primary");
        assert!(!primary.is_dirty());
        primary.advance_game_time();
        assert!(primary.is_dirty());
        if primary_first {
            primary.save().await.expect("save primary");
            derived.save().await.expect("save derived");
        } else {
            derived.save().await.expect("save derived");
            primary.save().await.expect("save primary");
        }
        let saved: toml::Table = toml::from_str(
            &fs::read_to_string(derived_dir.join("level.toml"))
                .await
                .expect("read"),
        )
        .expect("table");
        assert!(!saved.contains_key("game_time"));
        let reloaded = load(&primary_dir, GameTimeSource::Primary)
            .await
            .expect("reload primary");
        assert_eq!(reloaded.game_time_handle().ticks(), 1235);
        let derived = load(
            &derived_dir,
            GameTimeSource::Derived(reloaded.game_time_handle()),
        )
        .await
        .expect("reload derived without legacy field");
        assert_eq!(derived.game_time_handle().ticks(), 1235);
        assert!(!derived.is_dirty());
        assert!(
            load(&derived_dir, GameTimeSource::Primary).await.is_err(),
            "promotion cannot invent authority"
        );
        fs::remove_dir_all(primary_dir).await.expect("cleanup");
        fs::remove_dir_all(derived_dir).await.expect("cleanup");
    }
}

#[tokio::test]
async fn game_time_derived_ignores_all_legacy_types_but_validates_other_fields() {
    init_vanilla_registry();
    let dir = temp_level_data_dir("legacy-types");
    let clock = Arc::new(GameTime::new(456));
    for value in [
        None,
        Some("obsolete".into()),
        Some(true.into()),
        Some(1.5.into()),
        Some(toml::Value::Array(vec![1.into()])),
    ] {
        write_time(&dir, value).await;
        assert!(load(&dir, GameTimeSource::Primary).await.is_err());
        let derived = load(&dir, GameTimeSource::Derived(Arc::clone(&clock)))
            .await
            .expect("legacy value ignored");
        assert_eq!(derived.game_time_handle().ticks(), 456);
    }
    write_time(&dir, Some("obsolete".into())).await;
    let content = fs::read_to_string(dir.join("level.toml"))
        .await
        .expect("read");
    fs::write(
        dir.join("level.toml"),
        content.replace("seed = 7", "seed = false"),
    )
    .await
    .expect("corrupt seed");
    assert!(load(&dir, GameTimeSource::Derived(clock)).await.is_err());
    fs::remove_dir_all(dir).await.expect("cleanup");
}

#[tokio::test]
async fn game_time_new_and_ephemeral_primaries_are_independent_and_wrap() {
    init_vanilla_registry();
    let dir = temp_level_data_dir("new-clock");
    let primary = load(&dir, GameTimeSource::Primary)
        .await
        .expect("new primary");
    assert_eq!(primary.game_time_handle().ticks(), 0);
    write_time(&dir, Some(i64::MAX.into())).await;
    let mut primary = load(&dir, GameTimeSource::Primary)
        .await
        .expect("existing primary");
    let ephemeral = LevelDataManager::new(
        None::<&Path>,
        7,
        Difficulty::Normal,
        settings(
            "minecraft:overworld",
            vanilla_dimension_types::OVERWORLD.height,
        ),
        GameTimeSource::Primary,
    )
    .await
    .expect("ephemeral primary");
    primary.advance_game_time();
    assert_eq!(primary.game_time_handle().ticks(), i64::MIN);
    assert_eq!(ephemeral.game_time_handle().ticks(), 0);
    primary.save().await.expect("save wrapped value");
    assert_eq!(
        load(&dir, GameTimeSource::Primary)
            .await
            .expect("reload")
            .game_time_handle()
            .ticks(),
        i64::MIN
    );
    fs::remove_dir_all(dir).await.expect("cleanup");
}
