use ron::Map;

use super::*;

#[derive(Resource, Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
struct Ship {
    name: String,
    gas_kg: f64,
    seed: u64,
}

#[derive(Resource, Default)]
struct Applied(Vec<&'static str>);

fn capture_ship(world: &mut World) -> Option<Ship> {
    world.get_resource::<Ship>().cloned()
}

fn apply_ship(world: &mut World, ship: Ship, report: &mut Vec<String>) {
    report.push(format!("ship {}", ship.name));
    world.insert_resource(ship);
    world.resource_mut::<Applied>().0.push("ship");
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
struct Clock(f64);

fn capture_clock(world: &mut World) -> Option<Clock> {
    world.get_resource::<Time>().map(|_| Clock(12.5))
}

fn apply_clock(world: &mut World, _: Clock, _: &mut Vec<String>) {
    world.resource_mut::<Applied>().0.push("clock");
}

/// Version 3 of `ship`: version 1 named the gas `gas`, version 2 kept it in grams.
fn app() -> App {
    let mut app = App::new();
    app.init_resource::<Applied>()
        .init_resource::<Time>()
        .register_save_section(
            SaveSection::new("ship", 3, capture_ship, apply_ship)
                .migration(1, rename_gas)
                .migration(2, grams_to_kg),
        )
        .register_save_section(SaveSection::new("clock", 1, capture_clock, apply_clock));
    app
}

fn fields(value: Value) -> Result<Map, String> {
    match value {
        Value::Map(map) => Ok(map),
        other => Err(format!("not a struct: {other:?}")),
    }
}

fn rename_gas(value: Value) -> Result<Value, String> {
    let mut map = fields(value)?;
    if let Some(gas) = map.remove(&Value::String("gas".into())) {
        map.insert(Value::String("gas_g".into()), gas);
    }
    Ok(Value::Map(map))
}

fn grams_to_kg(value: Value) -> Result<Value, String> {
    let mut map = fields(value)?;
    let grams = map
        .remove(&Value::String("gas_g".into()))
        .ok_or("no gas_g")?;
    let Value::Number(grams) = grams else {
        return Err("gas_g is no number".into());
    };
    map.insert(
        Value::String("gas_kg".into()),
        Value::from(grams.into_f64() / 1000.0),
    );
    Ok(Value::Map(map))
}

fn saved(text: &str) -> SavedSections {
    SavedSections::from_ron(text.as_bytes()).unwrap()
}

#[test]
fn sections_round_trip_through_ron() {
    let mut app = app();
    let ship = Ship {
        name: "Skiff".into(),
        gas_kg: 812.25,
        seed: u64::MAX - 3,
    };
    app.world_mut().insert_resource(ship.clone());
    let text = capture_sections(app.world_mut())
        .serialize()
        .unwrap()
        .to_ron()
        .unwrap();
    assert!(text.contains("\"ship\""), "{text}");

    let mut other = self::app();
    let checked = check_sections(other.world(), &saved(&text)).unwrap();
    assert!(checked.skipped.is_empty());
    assert_eq!(checked.get::<Ship>("ship"), Some(&ship));
    let mut report = Vec::new();
    apply_sections(other.world_mut(), checked, &mut report);
    assert_eq!(other.world().resource::<Ship>(), &ship);
    assert_eq!(report, ["ship Skiff"]);
}

#[test]
fn sections_apply_in_registration_order() {
    let mut app = app();
    app.world_mut().insert_resource(Ship::default());
    let sections = capture_sections(app.world_mut()).serialize().unwrap();
    // Stored by name, so `clock` comes first in the file.
    assert_eq!(
        sections.sections.keys().collect::<Vec<_>>(),
        ["clock", "ship"]
    );
    let checked = check_sections(app.world(), &sections).unwrap();
    apply_sections(app.world_mut(), checked, &mut Vec::new());
    assert_eq!(app.world().resource::<Applied>().0, ["ship", "clock"]);
}

#[test]
fn a_capture_without_a_value_saves_no_section() {
    let mut app = app();
    let sections = capture_sections(app.world_mut()).serialize().unwrap();
    assert_eq!(sections.sections.keys().collect::<Vec<_>>(), ["clock"]);
}

#[test]
fn older_versions_migrate_in_order() {
    let app = app();
    let v1 = r#"(format: 1, sections: {"ship": (version: 1, value: (name: "Old", gas: 1500.0))})"#;
    let checked = check_sections(app.world(), &saved(v1)).unwrap();
    assert_eq!(
        checked.get::<Ship>("ship"),
        Some(&Ship {
            name: "Old".into(),
            gas_kg: 1.5,
            seed: 0
        })
    );
    let v2 = r#"(format: 1, sections: {"ship": (version: 2, value: (name: "Mid", gas_g: 250.0))})"#;
    let checked = check_sections(app.world(), &saved(v2)).unwrap();
    assert_eq!(checked.get::<Ship>("ship").unwrap().gas_kg, 0.25);
}

#[test]
fn a_failed_migration_refuses_the_save() {
    let app = app();
    let text = r#"(format: 1, sections: {"ship": (version: 2, value: (name: "Mid"))})"#;
    let error = check_sections(app.world(), &saved(text)).unwrap_err();
    assert!(
        matches!(&error, SectionError::Migration { section, from: 2, .. } if section == "ship"),
        "{error}"
    );
    assert_eq!(error.kind(), "migration_failed");
}

#[test]
fn a_version_without_its_migration_is_refused() {
    let mut app = App::new();
    app.register_save_section(SaveSection::new("ship", 2, capture_ship, apply_ship));
    let text = r#"(format: 1, sections: {"ship": (version: 1, value: ())})"#;
    assert_eq!(
        check_sections(app.world(), &saved(text)).unwrap_err(),
        SectionError::NoMigration {
            section: "ship".into(),
            from: 1
        }
    );
}

#[test]
fn newer_versions_and_formats_are_refused() {
    let app = app();
    let text = r#"(format: 1, sections: {"ship": (version: 4, value: ())})"#;
    assert_eq!(
        check_sections(app.world(), &saved(text)).unwrap_err(),
        SectionError::NewerVersion {
            section: "ship".into(),
            found: 4,
            reads: 3
        }
    );
    assert_eq!(
        SavedSections::from_ron(b"(format: 2, sections: {}, more: 1)").unwrap_err(),
        SectionError::NewerFormat { found: 2, reads: 1 }
    );
    assert_eq!(
        SavedSections::from_ron(b"(sections: {})").unwrap_err(),
        SectionError::NoFormat
    );
    assert_eq!(
        SavedSections::from_ron(b"(format: 1").unwrap_err().kind(),
        "unreadable"
    );
}

#[test]
fn a_bad_section_fails_the_whole_check() {
    let mut app = app();
    let text = r#"(format: 1, sections: {
        "clock": (version: 1, value: (12.5)),
        "ship": (version: 3, value: (name: 7)),
    })"#;
    let error = check_sections(app.world(), &saved(text)).unwrap_err();
    assert!(matches!(&error, SectionError::Invalid { section, .. } if section == "ship"));
    // Nothing was applied: a check changes nothing.
    assert!(app.world_mut().resource::<Applied>().0.is_empty());
}

#[test]
fn unknown_fields_are_skipped_and_reported() {
    let app = app();
    let current = r#"(format: 1, sections: {"ship": (version: 3, value: (name: "A", wings: 2))})"#;
    let checked = check_sections(app.world(), &saved(current)).unwrap();
    assert_eq!(checked.skipped, ["ship: wings"]);
    assert_eq!(checked.get::<Ship>("ship").unwrap().name, "A");
    let migrated = r#"(format: 1, sections: {"ship": (version: 2, value: (gas_g: 1.0, fins: 4))})"#;
    let checked = check_sections(app.world(), &saved(migrated)).unwrap();
    assert_eq!(checked.skipped, ["ship: fins"]);
}

#[test]
fn unknown_sections_are_kept_and_written_back() {
    let mut app = app();
    let text = r#"(format: 1, sections: {"cargo": (version: 7, value: (crates: [1, 2]))})"#;
    let checked = check_sections(app.world(), &saved(text)).unwrap();
    assert!(!checked.contains("cargo"));
    assert_eq!(
        checked.unknown.keys().collect::<Vec<_>>(),
        ["cargo"],
        "{checked:?}"
    );
    let mut next = capture_sections(app.world_mut()).serialize().unwrap();
    next.keep(&checked.unknown);
    let text = next.to_ron().unwrap();
    let again = saved(&text);
    let cargo = &again.sections["cargo"];
    assert_eq!(cargo.version, 7);
    assert_eq!(cargo.value.get_ron(), "(crates: [1, 2])");
}

#[test]
#[should_panic(expected = "registered twice")]
fn a_name_registers_once() {
    let mut app = app();
    app.register_save_section(SaveSection::new("clock", 1, capture_clock, apply_clock));
}
