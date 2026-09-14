//! Native management commands, dispatched before Tauri creates any windows.
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::PathBuf;

use crate::commands;
use anyhow::{Context, bail};
use codex_plus_core::settings::{AggregateRelayProfile, BackendSettings, RelayMode, SettingsStore};
use serde_json::{Value, json};

pub const COMMANDS: &[&str] = &[
    "help",
    "status",
    "settings-get",
    "settings-set",
    "providers-list",
    "provider-get",
    "provider-update",
    "model-routes-list",
    "model-route-set",
    "provider-switch",
    "providers-reorder",
    "aggregate-get",
    "aggregate-update",
    "aggregate-switch",
    "manager-open",
    "codex-start",
    "codex-restart",
];

struct Options {
    command: String,
    input: Value,
    state_dir: PathBuf,
    home: PathBuf,
    isolated: bool,
    include_secrets: bool,
    dry_run: bool,
}

pub fn run() -> i32 {
    let result = parse_options().and_then(|options| {
        let mut output = execute(&options)?;
        if !options.include_secrets {
            redact(&mut output);
        }
        Ok(output)
    });
    let (output, code) = match result {
        Ok(value) => (value, 0),
        Err(error) => (json!({"status":"failed","message":error.to_string()}), 1),
    };
    let mut stdout = std::io::stdout().lock();
    if serde_json::to_writer(&mut stdout, &output).is_err()
        || writeln!(stdout).is_err()
        || stdout.flush().is_err()
    {
        return 1;
    }
    code
}

fn parse_options() -> anyhow::Result<Options> {
    let mut args = std::env::args().skip(2);
    let command = args.next().unwrap_or_else(|| "help".into());
    let mut input_text = None;
    let (mut state_dir, mut home) = (None, None);
    let (mut include_secrets, mut dry_run) = (false, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" => {
                if input_text.is_some() {
                    bail!("Only one --input is allowed");
                }
                let value = args.next().context("--input requires JSON, @file or -")?;
                input_text = Some(if value == "-" {
                    let mut text = String::new();
                    std::io::stdin().read_to_string(&mut text)?;
                    text
                } else if let Some(path) = value.strip_prefix('@') {
                    std::fs::read_to_string(path)?
                } else {
                    value
                });
            }
            "--state-dir" => {
                state_dir = Some(PathBuf::from(
                    args.next().context("Missing state directory")?,
                ))
            }
            "--codex-home" => {
                home = Some(PathBuf::from(args.next().context("Missing Codex home")?))
            }
            "--include-secrets" => include_secrets = true,
            "--dry-run" => dry_run = true,
            _ => bail!("Unknown CLI option: {arg}"),
        }
    }
    if !COMMANDS.contains(&command.as_str()) {
        bail!("Unknown command: {command}; use --cli help");
    }
    if state_dir.is_some() != home.is_some() {
        bail!("Use --state-dir and --codex-home together");
    }
    let input: Value = match input_text {
        Some(text) => serde_json::from_str(text.trim_start_matches('\u{feff}'))
            .context("Invalid input JSON")?,
        None => json!({}),
    };
    if !input.is_object() {
        bail!("Input must be a JSON object");
    }
    Ok(Options {
        command,
        input,
        isolated: state_dir.is_some(),
        include_secrets,
        dry_run,
        state_dir: state_dir.unwrap_or_else(codex_plus_core::paths::default_app_state_dir),
        home: home.unwrap_or_else(codex_plus_core::relay_config::default_codex_home_dir),
    })
}

fn read_settings(options: &Options) -> anyhow::Result<(SettingsStore, BackendSettings, Value)> {
    let path = options.state_dir.join("settings.json");
    let store = SettingsStore::new(path.clone());
    let raw = if path.exists() {
        let text = std::fs::read_to_string(&path)?;
        let raw: Value =
            serde_json::from_str(&text).context("Invalid settings.json; no changes made")?;
        let _: BackendSettings =
            serde_json::from_value(raw.clone()).context("Invalid settings schema")?;
        raw
    } else {
        serde_json::to_value(BackendSettings::default())?
    };
    let settings = store.load()?;
    Ok((store, settings, raw))
}

fn execute(options: &Options) -> anyhow::Result<Value> {
    if options.command == "help" {
        return Ok(json!({
            "status":"ok","apiVersion":1,"commands":COMMANDS,
            "input":"--input JSON | --input @file.json | --input - (stdin)",
            "options":["--include-secrets","--dry-run","--state-dir PATH --codex-home PATH"],
            "examples":{
                "settings-set":{"patch":{"relayTestModel":"gpt-6-astra"}},
                "provider-switch":{"id":"saved-provider-id"},
                "provider-update":{"id":"saved-provider-id","patch":{"name":"Updated"}},
                "model-routes-list":{"id":"saved-provider-id"},
                "model-route-set":{"id":"saved-provider-id","model":"gpt-5.6-terra","enabled":false,"durationSeconds":18000},
                "providers-reorder":{"ids":["provider-b","provider-a"]},
                "aggregate-update":{"id":"aggregate-id","patch":{"strategy":"priorityFallback","members":[{"relayId":"provider-a","weight":1}]}},
                "codex-start":{"debugPort":9229,"helperPort":57321},
                "codex-restart":{"debugPort":9229,"helperPort":57321,"syncActiveRelay":true}
            }
        }));
    }
    if matches!(
        options.command.as_str(),
        "manager-open" | "codex-start" | "codex-restart"
    ) {
        return lifecycle(options);
    }
    let store = SettingsStore::new(options.state_dir.join("settings.json"));
    if options.command == "model-routes-list" {
        return Ok(serde_json::to_value(
            store.model_routes_list(string(&options.input, "id")?)?,
        )?);
    }
    if options.command == "model-route-set" {
        let request = serde_json::from_value::<codex_plus_core::settings::SetRelayModelRouteRequest>(
            options.input.clone(),
        )?;
        return Ok(serde_json::to_value(
            store.model_route_set(&request, options.dry_run)?,
        )?);
    }
    let writes = matches!(
        options.command.as_str(),
        "settings-set"
            | "provider-update"
            | "provider-switch"
            | "providers-reorder"
            | "aggregate-update"
            | "aggregate-switch"
    );
    let _lock = if writes && !options.dry_run {
        Some(store.control_lock()?)
    } else {
        None
    };
    let (store, mut settings, raw) = read_settings(options)?;
    let previous = settings.active_relay_id.clone();
    match options.command.as_str() {
        "status" => {
            return Ok(json!({
                "status":"ok","version":env!("CARGO_PKG_VERSION"),"apiVersion":1,
                "stateDir":options.state_dir,"codexHome":options.home,
                "activeRelayId":settings.active_relay_id,
                "activeAggregateRelayId":settings.active_aggregate_relay_profile().map(|p|p.id),
                "configured":codex_plus_core::relay_config::relay_config_status_from_home(&options.home).configured,
                "latestLaunch":codex_plus_core::status::StatusStore::new(options.state_dir.join("latest-status.json")).load_latest()?,
            }));
        }
        "settings-get" => {
            return Ok(
                json!({"status":"ok","settings":settings,"settingsPath":options.state_dir.join("settings.json")}),
            );
        }
        "providers-list" => {
            return Ok(
                json!({"status":"ok","activeRelayId":settings.active_relay_id,
            "providers":settings.relay_profiles,"aggregates":settings.aggregate_relay_profiles}),
            );
        }
        "provider-get" => {
            let id = string(&options.input, "id")?;
            let profile = settings
                .relay_profiles
                .iter()
                .find(|p| p.id == id)
                .context("Provider not found")?;
            return Ok(
                json!({"status":"ok","provider":profile,"active":settings.active_relay_id==id}),
            );
        }
        "aggregate-get" => {
            let id = string(&options.input, "id")?;
            let aggregate = settings
                .aggregate_relay_profiles
                .iter()
                .find(|p| p.id == id)
                .context("Aggregate not found")?;
            let profile = settings
                .relay_profiles
                .iter()
                .find(|p| p.id == id)
                .context("Aggregate profile not found")?;
            return Ok(
                json!({"status":"ok","aggregate":aggregate,"profile":profile,"active":settings.active_relay_id==id}),
            );
        }
        "settings-set" => {
            let patch = object(&options.input, "patch")?;
            for key in [
                "activeRelayId",
                "activeAggregateRelayId",
                "relayProfiles",
                "aggregateRelayProfiles",
            ] {
                if patch.get(key).is_some() {
                    bail!("Use provider/aggregate commands to change {key}");
                }
            }
            let mut next = serde_json::to_value(&settings)?;
            merge_patch(&mut next, patch)?;
            settings = serde_json::from_value(next)?;
        }
        "provider-update" => {
            let id = string(&options.input, "id")?;
            let patch = object(&options.input, "patch")?;
            if patch.get("id").is_some() || patch.get("relayMode").is_some() {
                bail!("Changing provider id or relayMode is not supported by provider-update");
            }
            let profile = settings
                .relay_profiles
                .iter_mut()
                .find(|p| p.id == id)
                .context("Provider not found")?;
            let mut next = serde_json::to_value(&*profile)?;
            for key in patch.as_object().unwrap().keys() {
                if next.get(key).is_none()
                    && ![
                        "model",
                        "baseUrl",
                        "apiKey",
                        "userAgent",
                        "modelRoutes",
                        "modelWindows",
                        "modelVlm",
                        "vlmApiKey",
                        "sub2apiMultiplier",
                    ]
                    .contains(&key.as_str())
                {
                    bail!("Unknown provider field: {key}");
                }
            }
            merge_object(&mut next, patch);
            *profile = serde_json::from_value(next)?;
            update_connection_fields(profile, patch)?;
            codex_plus_core::relay_config::normalize_relay_profile_for_storage(profile)?;
            if profile.relay_mode == RelayMode::Aggregate {
                if let Some(aggregate) = settings
                    .aggregate_relay_profiles
                    .iter_mut()
                    .find(|a| a.id == id)
                {
                    aggregate.name = profile.name.clone();
                }
            }
        }
        "provider-switch" | "aggregate-switch" => {
            let id = string(&options.input, "id")?;
            let profile = settings
                .relay_profiles
                .iter()
                .find(|p| p.id == id)
                .context("Provider not found")?;
            if options.command == "aggregate-switch" && profile.relay_mode != RelayMode::Aggregate {
                bail!("Selected provider is not an aggregate");
            }
            settings.active_aggregate_relay_id = if profile.relay_mode == RelayMode::Aggregate {
                id.into()
            } else {
                String::new()
            };
            settings.active_relay_id = id.into();
        }
        "providers-reorder" => {
            let ids: Vec<String> = serde_json::from_value(
                options.input.get("ids").context("ids is required")?.clone(),
            )?;
            let mut seen = HashSet::new();
            let mut ordered = Vec::new();
            for id in &ids {
                if !seen.insert(id) {
                    bail!("Duplicate provider id: {id}");
                }
                ordered.push(
                    settings
                        .relay_profiles
                        .iter()
                        .find(|p| &p.id == id)
                        .context("Unknown provider in order")?
                        .clone(),
                );
            }
            ordered.extend(
                settings
                    .relay_profiles
                    .iter()
                    .filter(|p| !seen.contains(&p.id))
                    .cloned(),
            );
            settings.relay_profiles = ordered;
        }
        "aggregate-update" => {
            let id = string(&options.input, "id")?;
            let patch = object(&options.input, "patch")?;
            if patch.get("id").is_some() {
                bail!("Aggregate id cannot change");
            }
            let aggregate = settings
                .aggregate_relay_profiles
                .iter_mut()
                .find(|p| p.id == id)
                .context("Aggregate not found")?;
            let mut next = serde_json::to_value(&*aggregate)?;
            merge_patch(&mut next, patch)?;
            *aggregate = serde_json::from_value::<AggregateRelayProfile>(next)?;
            if let Some(name) = patch.get("name") {
                settings
                    .relay_profiles
                    .iter_mut()
                    .find(|p| p.id == id)
                    .context("Aggregate profile not found")?
                    .name = name.as_str().context("name must be a string")?.into();
            }
        }
        _ => unreachable!(),
    }
    validate(&settings)?;
    let settings = commands::normalize_settings_before_save(settings);
    let should_apply = settings.relay_profiles_enabled
        && (options.command == "settings-set"
            || options.command.ends_with("-switch")
            || options.command == "providers-reorder"
            || string(&options.input, "id").ok() == Some(settings.active_relay_id.as_str())
            || settings.active_aggregate_relay_profile().is_some_and(|a| {
                a.members
                    .iter()
                    .any(|m| Some(m.relay_id.as_str()) == string(&options.input, "id").ok())
            }));
    if options.dry_run {
        return Ok(
            json!({"status":"ok","dryRun":true,"wouldApplyLiveConfig":should_apply,"settings":settings}),
        );
    }
    if options.command.ends_with("-switch") && !settings.relay_profiles_enabled {
        bail!("Provider configuration is disabled");
    }
    let backup = if should_apply {
        match codex_plus_core::relay_switch::switch_relay_profile_in_home(
            &store,
            &options.home,
            settings,
            &previous,
        ) {
            Ok(result) => result.backup_path,
            Err(error) => {
                codex_plus_core::settings::atomic_write(
                    &options.state_dir.join("settings.json"),
                    &serde_json::to_vec_pretty(&raw)?,
                )
                .context("Failed to restore original settings after switch failure")?;
                return Err(error);
            }
        }
    } else {
        store.save(&settings)?;
        None
    };
    // Keep top-level fields belonging to another installed version.
    let normalized = store.load()?;
    let mut saved = raw;
    for (key, value) in serde_json::to_value(&normalized)?.as_object().unwrap() {
        saved[key] = value.clone();
    }
    codex_plus_core::settings::atomic_write(
        &options.state_dir.join("settings.json"),
        &serde_json::to_vec_pretty(&saved)?,
    )?;
    Ok(
        json!({"status":"ok","appliedLiveConfig":should_apply,"restartRequested":false,
        "settings":normalized,"backupPath":backup,"configPath":options.home.join("config.toml")}),
    )
}

fn validate(settings: &BackendSettings) -> anyhow::Result<()> {
    let mut ids = HashSet::new();
    for profile in &settings.relay_profiles {
        if profile.id.is_empty() || !ids.insert(profile.id.as_str()) {
            bail!("Duplicate or empty provider id");
        }
    }
    if !ids.contains(settings.active_relay_id.as_str()) {
        bail!("Active provider does not exist");
    }
    for aggregate in &settings.aggregate_relay_profiles {
        if !settings
            .relay_profiles
            .iter()
            .any(|p| p.id == aggregate.id && p.relay_mode == RelayMode::Aggregate)
        {
            bail!(
                "Aggregate has no matching provider profile: {}",
                aggregate.id
            );
        }
        let mut members = HashSet::new();
        if aggregate.members.is_empty() {
            bail!("Aggregate must have a member: {}", aggregate.id);
        }
        for member in &aggregate.members {
            if member.weight == 0
                || !members.insert(&member.relay_id)
                || !settings
                    .relay_profiles
                    .iter()
                    .any(|p| p.id == member.relay_id && p.relay_mode != RelayMode::Aggregate)
            {
                bail!("Invalid or duplicate aggregate member: {}", member.relay_id);
            }
        }
    }
    Ok(())
}

fn lifecycle(options: &Options) -> anyhow::Result<Value> {
    if options.command == "manager-open" {
        let exe = std::env::current_exe()?;
        if options.dry_run {
            return Ok(json!({"status":"ok","dryRun":true,"executable":exe,"args":[]}));
        }
        if options.isolated {
            bail!("manager-open is dry-run only with isolated paths");
        }
        let target = codex_plus_core::install::spawn_companion(
            codex_plus_core::install::MANAGER_BINARY,
            std::iter::empty::<&str>(),
        )?;
        return Ok(json!({"status":"accepted","path":target,"readiness":"not_checked"}));
    }
    let (_, settings, _) = read_settings(options)?;
    let mut input = options.input.clone();
    if input.get("appPath").is_none() {
        input["appPath"] = json!(settings.codex_app_path);
    }
    if input.get("syncActiveRelay").is_none() {
        input["syncActiveRelay"] = json!(true);
    }
    let request: commands::LaunchRequest = serde_json::from_value(input)?;
    let launcher =
        codex_plus_core::install::companion_binary_path(codex_plus_core::install::SILENT_BINARY);
    if !launcher.is_file() {
        bail!("Companion launcher not found: {}", launcher.display());
    }
    if options.dry_run {
        return Ok(
            json!({"status":"ok","dryRun":true,"operation":options.command,"launcher":launcher,
            "appPath":request.app_path,"debugPort":request.debug_port,"helperPort":request.helper_port,
            "syncActiveRelay":request.sync_active_relay,"launchIncludesInjection":true,
            "willInterruptRunningTasks":options.command=="codex-restart"}),
        );
    }
    if options.isolated {
        bail!("Lifecycle commands are dry-run only with isolated configuration paths");
    }
    if options.command == "codex-start"
        && request.sync_active_relay
        && settings.relay_profiles_enabled
    {
        let _lock = SettingsStore::default().control_lock()?;
        commands::sync_active_relay_to_home(&settings, &options.home)?;
    }
    let result = if options.command == "codex-restart" {
        commands::restart_codex_plus(request)
    } else {
        commands::launch_codex_plus(request)
    };
    if result.status == "failed" {
        bail!("{}", result.message);
    }
    Ok(serde_json::to_value(result)?)
}

fn update_connection_fields(
    profile: &mut codex_plus_core::settings::RelayProfile,
    patch: &Value,
) -> anyhow::Result<()> {
    let url = patch
        .get("baseUrl")
        .or_else(|| patch.get("upstreamBaseUrl"));
    let key = patch.get("apiKey");
    let model = patch.get("model");
    if url.is_none() && key.is_none() && model.is_none() {
        return Ok(());
    }
    if profile.relay_mode == RelayMode::Aggregate {
        bail!(
            "Aggregate connection is managed by Codex++; update its modelList or members instead"
        );
    }
    let mut doc = profile.config_contents.parse::<toml_edit::DocumentMut>()?;
    if let Some(model) = model {
        doc["model"] = toml_edit::value(model.as_str().context("model must be a string")?);
    }
    if url.is_some() || key.is_some() {
        let provider_id = doc
            .get("model_provider")
            .and_then(toml_edit::Item::as_str)
            .unwrap_or("custom")
            .to_string();
        let tables = doc
            .get_mut("model_providers")
            .and_then(toml_edit::Item::as_table_like_mut)
            .context("Provider config has no model_providers table; update configContents")?;
        let transport_id = if tables
            .get(&provider_id)
            .and_then(toml_edit::Item::as_table_like)
            .is_some_and(|p| p.get("base_url").is_some())
        {
            provider_id
        } else {
            "custom".to_string()
        };
        let table = tables
            .get_mut(&transport_id)
            .and_then(toml_edit::Item::as_table_like_mut)
            .context("Provider transport table not found")?;
        if let Some(url) = url {
            let url = url.as_str().context("baseUrl must be a string")?;
            table.insert("base_url", toml_edit::value(url));
            profile.base_url = url.into();
            profile.upstream_base_url = url.into();
        }
        if let Some(key) = key {
            let key = key.as_str().context("apiKey must be a string")?;
            profile.api_key = key.into();
            if profile.relay_mode == RelayMode::PureApi {
                let mut auth: Value = if profile.auth_contents.trim().is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(&profile.auth_contents)?
                };
                auth["OPENAI_API_KEY"] = json!(key);
                profile.auth_contents = serde_json::to_string_pretty(&auth)?;
            } else {
                table.insert("experimental_bearer_token", toml_edit::value(key));
            }
        }
    }
    profile.config_contents = doc.to_string();
    Ok(())
}

fn string<'a>(value: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .with_context(|| format!("{key} must be a non-empty string"))
}
fn object<'a>(value: &'a Value, key: &str) -> anyhow::Result<&'a Value> {
    value
        .get(key)
        .filter(|v| v.is_object())
        .with_context(|| format!("{key} must be an object"))
}
fn merge_patch(target: &mut Value, patch: &Value) -> anyhow::Result<()> {
    for key in patch.as_object().context("patch must be an object")?.keys() {
        if target.get(key).is_none() {
            bail!("Unknown setting: {key}");
        }
    }
    merge_object(target, patch);
    Ok(())
}
fn merge_object(target: &mut Value, patch: &Value) {
    for (key, value) in patch.as_object().unwrap() {
        target[key] = value.clone();
    }
}
fn redact(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                let key = key.to_ascii_lowercase();
                if key.contains("apikey")
                    || key.contains("apisecret")
                    || key.ends_with("token")
                    || key.contains("password")
                    || key.contains("authcontents")
                    || key.contains("configcontents")
                {
                    if !value.is_null() && value != &json!("") {
                        *value = json!("[redacted]");
                    }
                } else {
                    redact(value);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact(item);
            }
        }
        _ => {}
    }
}
