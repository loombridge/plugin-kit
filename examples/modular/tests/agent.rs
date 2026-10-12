//! Agents address Example Modular's parameters by name and path: a slot's
//! type and its new effect's parameters set in one call, reaching the host
//! as gestures, and `params.get` showing each parameter as it is now.

use lspk_example_modular::{Modular, effects, slot_param, slot_type};
use lspk_plugin::{OutputEvent, Plugin};
use lspk_plugin_agent::WithAgent;
use lspk_test::render::{Instance, Settings};
use serde_json::{Value, json};

lspk_rt::install_trap!();

fn names(params: &Value) -> Vec<String> {
    params["params"]
        .as_array()
        .expect("parameters")
        .iter()
        .map(|p| p["name"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn an_agent_changes_a_slots_effect_and_its_parameters_in_one_call() {
    let mut instance = Instance::<WithAgent<Modular>>::new().expect("an instance");
    instance
        .activate(Settings::new(48_000.0, 256))
        .expect("activation");
    let plugin = instance.plugin_mut();

    let all = plugin
        .run_command("params.get", &json!({}))
        .expect("parameters");
    let listed = names(&all);
    assert_eq!(
        listed.len(),
        12,
        "the slots' unused parameters are left out"
    );
    assert_eq!(all["params"][1]["path"], "osc \"A\" / param \"Level\"");

    // The type first, then the new effect's parameters, by path and name.
    let set = plugin
        .run_command(
            "params.set",
            &json!({ "values": [
                { "param": "fx \"1\" / param \"Type\"", "text": "Delay" },
                { "param": "fx \"1\" / param \"Time\"", "text": "500 ms" },
                { "param": "FX 1 Mix", "value": 50.0 },
            ] }),
        )
        .expect("the values set");
    assert_eq!(names(&set), ["FX 1 Type", "FX 1 Time", "FX 1 Mix"], "{set}");
    assert_eq!(set["params"][1]["text"], "500 ms");
    assert!((set["params"][1]["value"].as_f64().unwrap_or_default() - 500.0).abs() < 1e-6);
    assert_eq!(set["params"][2]["kind"]["continuous"]["unit"], "percent");

    let delay = plugin
        .run_command(
            "params.get",
            &json!({ "params": ["fx \"1\" / param \"Tone\""] }),
        )
        .expect("the tone");
    assert_eq!(delay["params"][0]["text"], "8.00 kHz", "at its default");
    assert_eq!(delay["params"][0]["min"], 200.0);
    let gone = plugin.run_command("params.get", &json!({ "params": ["FX 1 Drive"] }));
    assert!(gone.is_err(), "Drive isn't in slot 1 now");
    let hidden = plugin.run_command("params.get", &json!({ "params": [slot_param(1, 0).0] }));
    assert!(hidden.is_err(), "slot 2 holds nothing");

    // The host hears each change as a person's: a gesture around each
    // parameter the agent set, and none for the defaults the delay brought.
    let block = instance.process(256, &[], &[]);
    let begun: Vec<u32> = block
        .events
        .iter()
        .filter_map(|e| match e {
            OutputEvent::GestureBegin { id, .. } => Some(id.0),
            _ => None,
        })
        .collect();
    assert_eq!(
        begun,
        [slot_type(0).0, slot_param(0, 0).0, slot_param(0, 2).0]
    );
    let values = instance.values();
    assert_eq!(values.get(slot_type(0)), Some(effects::DELAY as f64));
    let tone = values.view(slot_param(0, 3)).expect("the tone");
    assert_eq!(
        tone.format(values.get(slot_param(0, 3)).unwrap_or(0.0)),
        "8.00 kHz"
    );

    let summary = instance.plugin().summary().expect("a summary");
    assert!(summary.contains("slot 1 holds Delay"), "{summary}");
}
