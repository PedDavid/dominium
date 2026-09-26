//! The `settings` blocks in the chart's values parse as the app's settings file.

use dominium::settings::Settings;

fn settings_from_values(path: &str) -> Settings {
    let raw = std::fs::read_to_string(path).unwrap();
    let values: serde_yaml::Value = serde_yaml::from_str(&raw).unwrap();
    let block = serde_yaml::to_string(&values["settings"]).unwrap();
    let file = std::env::temp_dir().join(format!(
        "dominium-chart-{}-{}.yaml",
        std::process::id(),
        path.replace('/', "_")
    ));
    std::fs::write(&file, block).unwrap();
    let settings = Settings::load(Some(&file)).unwrap();
    std::fs::remove_file(&file).ok();
    settings
}

#[test]
fn default_values_parse() {
    let s = settings_from_values("deploy/helm/dominium/values.yaml");
    assert_eq!(s, Settings::default());
}

#[test]
fn example_values_parse() {
    let s = settings_from_values("deploy/examples/values-homelab.yaml");
    assert_eq!(s.registrars["ptisp"].prices["pt"], 16.9);
    assert_eq!(s.registrar_name("cloudflare"), "Cloudflare");
}
