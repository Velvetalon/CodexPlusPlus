use codex_plus_core::relay_rotation::{
    RelayRequestOutcome, RelayRotationSelector, RotationContext, RotationEvent, SelectionError,
    fallback_relays_after, record_relay_request_failure, record_relay_request_outcome,
    reset_priority_fallback_cooldowns, select_relay_for_probe, select_relay_for_request,
};
use codex_plus_core::settings::{
    AggregateRelayMember, AggregateRelayProfile, AggregateRelayStrategy, BackendSettings,
    RelayMode, RelayProfile, RelaySessionProvider,
};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

fn global_selector_test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn profile(id: &str) -> RelayProfile {
    RelayProfile {
        id: id.to_string(),
        name: id.to_string(),
        base_url: format!("https://{id}.example/v1"),
        api_key: format!("sk-{id}"),
        ..RelayProfile::default()
    }
}

fn aggregate(strategy: AggregateRelayStrategy) -> AggregateRelayProfile {
    AggregateRelayProfile {
        id: "agg".to_string(),
        name: "聚合".to_string(),
        session_provider: RelaySessionProvider::Custom,
        code_mode_host: false,
        strategy,
        members: vec![
            AggregateRelayMember {
                relay_id: "relay-a".to_string(),
                weight: 1,
            },
            AggregateRelayMember {
                relay_id: "relay-b".to_string(),
                weight: 2,
            },
            AggregateRelayMember {
                relay_id: "relay-c".to_string(),
                weight: 1,
            },
        ],
    }
}

fn aggregate_with_id(id: &str, strategy: AggregateRelayStrategy) -> AggregateRelayProfile {
    AggregateRelayProfile {
        id: id.to_string(),
        name: "聚合".to_string(),
        session_provider: RelaySessionProvider::Custom,
        code_mode_host: false,
        strategy,
        members: vec![
            AggregateRelayMember {
                relay_id: "relay-a".to_string(),
                weight: 1,
            },
            AggregateRelayMember {
                relay_id: "relay-b".to_string(),
                weight: 2,
            },
        ],
    }
}

fn settings(strategy: AggregateRelayStrategy) -> BackendSettings {
    BackendSettings {
        relay_profiles: vec![
            profile("relay-a"),
            profile("relay-b"),
            profile("relay-c"),
            RelayProfile {
                id: "agg".to_string(),
                name: "聚合".to_string(),
                relay_mode: RelayMode::Aggregate,
                ..RelayProfile::default()
            },
        ],
        aggregate_relay_profiles: vec![aggregate(strategy)],
        active_relay_id: "agg".to_string(),
        active_aggregate_relay_id: "agg".to_string(),
        ..BackendSettings::default()
    }
}

fn trigger_cooldown(
    selector: &mut RelayRotationSelector,
    relay_id: &str,
    outcome: RelayRequestOutcome,
    now: Instant,
) {
    for _ in 0..3 {
        selector.record_outcome_at(relay_id, outcome, now);
    }
}

#[test]
fn failover_keeps_current_provider_until_failure_then_moves_to_next_member() {
    let settings = settings(AggregateRelayStrategy::Failover);
    let mut selector = RelayRotationSelector::from_settings(&settings).unwrap();

    let first = selector
        .select(&settings, RotationContext::for_conversation("chat-1"))
        .unwrap();
    selector.record_event(RotationEvent::Success);
    let second = selector
        .select(&settings, RotationContext::for_conversation("chat-1"))
        .unwrap();
    selector.record_event(RotationEvent::Failure);
    let third = selector
        .select(&settings, RotationContext::for_conversation("chat-1"))
        .unwrap();

    assert_eq!(first.id, "relay-a");
    assert_eq!(second.id, "relay-a");
    assert_eq!(third.id, "relay-b");
}

#[test]
fn priority_fallback_always_starts_each_request_with_first_member() {
    let settings = settings(AggregateRelayStrategy::PriorityFallback);
    let mut selector = RelayRotationSelector::from_settings(&settings).unwrap();
    let now = Instant::now();

    let first = selector
        .select_at(&settings, RotationContext::default(), now)
        .unwrap();
    selector.record_outcome_at(&first.id, RelayRequestOutcome::HttpStatus(200), now);
    let second = selector
        .select_at(&settings, RotationContext::default(), now)
        .unwrap();

    assert_eq!(first.id, "relay-a");
    assert_eq!(second.id, "relay-a");
}

#[test]
fn priority_fallback_429_cools_a_for_30_minutes_then_fails_back() {
    let settings = settings(AggregateRelayStrategy::PriorityFallback);
    let mut selector = RelayRotationSelector::from_settings(&settings).unwrap();
    let now = Instant::now();

    selector.record_outcome_at("relay-a", RelayRequestOutcome::HttpStatus(429), now);
    assert_eq!(selector.cooldown_until("relay-a"), None);
    assert_eq!(selector.consecutive_failures("relay-a"), 1);
    selector.record_outcome_at("relay-a", RelayRequestOutcome::HttpStatus(429), now);
    assert_eq!(selector.cooldown_until("relay-a"), None);
    assert_eq!(selector.consecutive_failures("relay-a"), 2);
    selector.record_outcome_at("relay-a", RelayRequestOutcome::HttpStatus(429), now);
    assert_eq!(selector.consecutive_failures("relay-a"), 0);

    assert_eq!(
        selector
            .select_at(
                &settings,
                RotationContext::default(),
                now + Duration::from_secs(30 * 60 - 1),
            )
            .unwrap()
            .id,
        "relay-b"
    );
    let recovered_at = now + Duration::from_secs(30 * 60);
    let recovered = selector
        .select_at(&settings, RotationContext::default(), recovered_at)
        .unwrap();
    assert_eq!(recovered.id, "relay-a");
    selector.record_outcome_at(
        &recovered.id,
        RelayRequestOutcome::HttpStatus(200),
        recovered_at,
    );
    assert_eq!(
        selector
            .select_at(
                &settings,
                RotationContext::default(),
                recovered_at + Duration::from_secs(1),
            )
            .unwrap()
            .id,
        "relay-a"
    );
}

#[test]
fn priority_fallback_502_and_503_cool_a_for_3_minutes() {
    let settings = settings(AggregateRelayStrategy::PriorityFallback);
    let now = Instant::now();

    for status in [502, 503] {
        let mut selector = RelayRotationSelector::from_settings(&settings).unwrap();
        trigger_cooldown(
            &mut selector,
            "relay-a",
            RelayRequestOutcome::HttpStatus(status),
            now,
        );

        assert_eq!(
            selector.cooldown_until("relay-a"),
            Some(now + Duration::from_secs(3 * 60))
        );
        assert_eq!(
            selector
                .select_at(
                    &settings,
                    RotationContext::default(),
                    now + Duration::from_secs(3 * 60 - 1),
                )
                .unwrap()
                .id,
            "relay-b"
        );
        assert_eq!(
            selector
                .select_at(
                    &settings,
                    RotationContext::default(),
                    now + Duration::from_secs(3 * 60),
                )
                .unwrap()
                .id,
            "relay-a"
        );
    }
}

#[test]
fn priority_fallback_uses_status_specific_and_transport_cooldowns() {
    let settings = settings(AggregateRelayStrategy::PriorityFallback);
    let now = Instant::now();
    for (outcome, expected) in [
        (
            RelayRequestOutcome::HttpStatus(402),
            Duration::from_secs(60 * 60),
        ),
        (
            RelayRequestOutcome::HttpStatus(418),
            Duration::from_secs(10 * 60),
        ),
        (
            RelayRequestOutcome::TransportFailure,
            Duration::from_secs(10 * 60),
        ),
    ] {
        let mut selector = RelayRotationSelector::from_settings(&settings).unwrap();
        trigger_cooldown(&mut selector, "relay-a", outcome, now);
        assert_eq!(selector.cooldown_until("relay-a"), Some(now + expected));
    }
}

#[test]
fn priority_fallback_success_resets_consecutive_failure_count() {
    let settings = settings(AggregateRelayStrategy::PriorityFallback);
    let mut selector = RelayRotationSelector::from_settings(&settings).unwrap();
    let now = Instant::now();

    assert!(!selector.record_outcome_at("relay-a", RelayRequestOutcome::HttpStatus(500), now));
    assert!(!selector.record_outcome_at("relay-a", RelayRequestOutcome::HttpStatus(500), now));
    assert_eq!(selector.consecutive_failures("relay-a"), 2);

    assert!(!selector.record_outcome_at("relay-a", RelayRequestOutcome::HttpStatus(200), now));
    assert_eq!(selector.consecutive_failures("relay-a"), 0);
    assert_eq!(selector.cooldown_until("relay-a"), None);
}

#[test]
fn priority_fallback_status_reports_remaining_time_count_and_last_reason() {
    let settings = settings(AggregateRelayStrategy::PriorityFallback);
    let mut selector = RelayRotationSelector::from_settings(&settings).unwrap();
    let now = Instant::now();

    assert!(!selector.record_outcome_at("relay-a", RelayRequestOutcome::HttpStatus(429), now));
    let pending = selector.cooldown_status_at(now);
    assert_eq!(pending.members[0].consecutive_failures, 1);
    assert_eq!(pending.members[0].cooldown_remaining_seconds, 0);
    assert_eq!(pending.members[0].last_cooldown_reason, None);

    assert!(!selector.record_outcome_at("relay-a", RelayRequestOutcome::HttpStatus(429), now));
    assert!(selector.record_outcome_at("relay-a", RelayRequestOutcome::HttpStatus(429), now));
    let cooling = selector.cooldown_status_at(now + Duration::from_secs(1));
    assert_eq!(cooling.members[0].consecutive_failures, 0);
    assert_eq!(cooling.members[0].failure_threshold, 3);
    assert_eq!(cooling.members[0].cooldown_remaining_seconds, 30 * 60 - 1);
    assert_eq!(
        cooling.members[0].last_cooldown_reason,
        Some(RelayRequestOutcome::HttpStatus(429))
    );

    selector.reset_priority_fallback_cooldowns();
    let reset = selector.cooldown_status_at(now + Duration::from_secs(1));
    assert_eq!(reset.members[0].cooldown_remaining_seconds, 0);
    assert_eq!(reset.members[0].consecutive_failures, 0);
    assert_eq!(
        reset.members[0].last_cooldown_reason,
        Some(RelayRequestOutcome::HttpStatus(429))
    );
}

#[test]
fn priority_fallback_tracks_member_cooldowns_independently_and_recovers_a_from_c() {
    let settings = settings(AggregateRelayStrategy::PriorityFallback);
    let mut selector = RelayRotationSelector::from_settings(&settings).unwrap();
    let now = Instant::now();

    trigger_cooldown(
        &mut selector,
        "relay-a",
        RelayRequestOutcome::HttpStatus(429),
        now,
    );
    trigger_cooldown(
        &mut selector,
        "relay-b",
        RelayRequestOutcome::HttpStatus(402),
        now,
    );

    assert_eq!(
        selector
            .select_at(&settings, RotationContext::default(), now)
            .unwrap()
            .id,
        "relay-c"
    );
    assert_eq!(selector.cooldown_until("relay-c"), None);
    assert_eq!(
        selector
            .select_at(
                &settings,
                RotationContext::default(),
                now + Duration::from_secs(30 * 60),
            )
            .unwrap()
            .id,
        "relay-a"
    );
}

#[test]
fn priority_fallback_forces_earliest_expiring_member_when_all_are_cooling_down() {
    let settings = settings(AggregateRelayStrategy::PriorityFallback);
    let mut selector = RelayRotationSelector::from_settings(&settings).unwrap();
    let now = Instant::now();

    trigger_cooldown(
        &mut selector,
        "relay-a",
        RelayRequestOutcome::HttpStatus(429),
        now,
    );
    trigger_cooldown(
        &mut selector,
        "relay-b",
        RelayRequestOutcome::HttpStatus(402),
        now,
    );
    trigger_cooldown(
        &mut selector,
        "relay-c",
        RelayRequestOutcome::HttpStatus(503),
        now,
    );

    assert_eq!(
        selector
            .select_at(&settings, RotationContext::default(), now)
            .unwrap()
            .id,
        "relay-c"
    );
    // 所有成员都在冷却时，不再返回其它冷却中的成员作为后备，
    // 避免协议代理顺延冷却时间造成无限重试。
    assert!(selector.priority_fallbacks_after("relay-c", now).is_empty());
    // relay-c 仍在原冷却中，失败不会继续延长冷却时间。
    trigger_cooldown(
        &mut selector,
        "relay-c",
        RelayRequestOutcome::HttpStatus(429),
        now,
    );
    assert_eq!(
        selector.cooldown_until("relay-c"),
        Some(now + Duration::from_secs(3 * 60))
    );
}

#[test]
fn priority_fallback_skips_cooled_members_in_fallback_chain() {
    let settings = settings(AggregateRelayStrategy::PriorityFallback);
    let mut selector = RelayRotationSelector::from_settings(&settings).unwrap();
    let now = Instant::now();

    // 冷却 relay-a，此时 relay-b、relay-c 可用。
    trigger_cooldown(
        &mut selector,
        "relay-a",
        RelayRequestOutcome::HttpStatus(429),
        now,
    );

    // select 应落到第一个可用成员 relay-b。
    assert_eq!(
        selector
            .select_at(&settings, RotationContext::default(), now)
            .unwrap()
            .id,
        "relay-b"
    );
    // fallback 链不应包含冷却中的 relay-a。
    assert_eq!(
        selector.priority_fallbacks_after("relay-b", now),
        vec!["relay-c"]
    );
}

#[test]
fn conversation_rotation_sticks_each_conversation_to_a_stable_member() {
    let settings = settings(AggregateRelayStrategy::ConversationRoundRobin);
    let mut selector = RelayRotationSelector::from_settings(&settings).unwrap();

    let chat_a_first = selector
        .select(&settings, RotationContext::for_conversation("chat-a"))
        .unwrap();
    let chat_a_second = selector
        .select(&settings, RotationContext::for_conversation("chat-a"))
        .unwrap();
    let chat_b_first = selector
        .select(&settings, RotationContext::for_conversation("chat-b"))
        .unwrap();

    assert_eq!(chat_a_first.id, "relay-a");
    assert_eq!(chat_a_second.id, "relay-a");
    assert_eq!(chat_b_first.id, "relay-b");
}

#[test]
fn request_rotation_advances_on_every_request() {
    let settings = settings(AggregateRelayStrategy::RequestRoundRobin);
    let mut selector = RelayRotationSelector::from_settings(&settings).unwrap();

    let selected = (0..5)
        .map(|_| {
            selector
                .select(&settings, RotationContext::default())
                .unwrap()
                .id
        })
        .collect::<Vec<_>>();

    assert_eq!(
        selected,
        vec!["relay-a", "relay-b", "relay-c", "relay-a", "relay-b"]
    );
}

#[test]
fn weighted_rotation_repeats_members_by_configured_weight() {
    let settings = settings(AggregateRelayStrategy::WeightedRoundRobin);
    let mut selector = RelayRotationSelector::from_settings(&settings).unwrap();

    let selected = (0..6)
        .map(|_| {
            selector
                .select(&settings, RotationContext::default())
                .unwrap()
                .id
        })
        .collect::<Vec<_>>();

    assert_eq!(
        selected,
        vec![
            "relay-a", "relay-b", "relay-b", "relay-c", "relay-a", "relay-b"
        ]
    );
}

#[test]
fn aggregate_members_must_reference_existing_relay_profiles() {
    let mut settings = settings(AggregateRelayStrategy::RequestRoundRobin);
    settings.aggregate_relay_profiles[0]
        .members
        .push(AggregateRelayMember {
            relay_id: "missing-relay".to_string(),
            weight: 1,
        });

    let error = RelayRotationSelector::from_settings(&settings).unwrap_err();

    assert_eq!(
        error,
        SelectionError::UnknownMemberRelay {
            aggregate_id: "agg".to_string(),
            relay_id: "missing-relay".to_string()
        }
    );
}

#[test]
fn aggregate_with_one_member_is_allowed_without_rotation() {
    let mut settings = settings(AggregateRelayStrategy::RequestRoundRobin);
    settings.aggregate_relay_profiles[0].members.truncate(1);

    let mut selector = RelayRotationSelector::from_settings(&settings).unwrap();
    let first = selector
        .select(&settings, RotationContext::default())
        .unwrap();
    let second = selector
        .select(&settings, RotationContext::default())
        .unwrap();

    assert_eq!(first.id, "relay-a");
    assert_eq!(second.id, "relay-a");
}

#[test]
fn aggregate_members_must_be_api_capable_relay_profiles() {
    let mut settings = settings(AggregateRelayStrategy::WeightedRoundRobin);
    settings.relay_profiles.push(RelayProfile {
        id: "official-login".to_string(),
        name: "官方登录".to_string(),
        base_url: String::new(),
        api_key: String::new(),
        ..RelayProfile::default()
    });
    settings.aggregate_relay_profiles[0]
        .members
        .push(AggregateRelayMember {
            relay_id: "official-login".to_string(),
            weight: 1,
        });

    let error = RelayRotationSelector::from_settings(&settings).unwrap_err();

    assert_eq!(
        error,
        SelectionError::InvalidMemberRelay {
            aggregate_id: "agg".to_string(),
            relay_id: "official-login".to_string()
        }
    );
}

#[test]
fn aggregate_members_accept_no_auth_pure_api_profiles() {
    let mut settings = settings(AggregateRelayStrategy::RequestRoundRobin);
    settings.relay_profiles[0] = RelayProfile {
        id: "relay-a".to_string(),
        name: "relay-a".to_string(),
        base_url: "https://relay-a.example/v1".to_string(),
        relay_mode: RelayMode::PureApi,
        no_auth: true,
        api_key: String::new(),
        ..RelayProfile::default()
    };

    let mut selector = RelayRotationSelector::from_settings(&settings).unwrap();
    let selected = selector
        .select(&settings, RotationContext::default())
        .unwrap();

    assert_eq!(selected.id, "relay-a");
    assert!(selected.uses_no_auth());
}

#[test]
fn select_relay_for_request_uses_active_relay_id_as_aggregate_source_of_truth() {
    let _guard = global_selector_test_lock();
    let mut settings = settings(AggregateRelayStrategy::WeightedRoundRobin);
    settings.active_relay_id = "agg".to_string();
    settings.active_aggregate_relay_id.clear();

    let selected = select_relay_for_request(&settings, RotationContext::default()).unwrap();

    assert_eq!(selected.id, "relay-a");
}

#[test]
fn select_relay_for_request_ignores_stale_active_aggregate_id_for_regular_relay() {
    let _guard = global_selector_test_lock();
    let mut settings = settings(AggregateRelayStrategy::WeightedRoundRobin);
    settings.active_relay_id = "relay-b".to_string();
    settings.active_aggregate_relay_id = "agg".to_string();

    let selected = select_relay_for_request(&settings, RotationContext::default()).unwrap();

    assert_eq!(selected.id, "relay-b");
}

#[test]
fn select_relay_for_request_resets_rotation_after_switching_to_regular_relay() {
    let _guard = global_selector_test_lock();
    let mut settings = settings(AggregateRelayStrategy::RequestRoundRobin);
    settings.active_relay_id = "agg".to_string();

    let first = select_relay_for_request(&settings, RotationContext::default()).unwrap();
    let mut regular_settings = settings.clone();
    regular_settings.active_relay_id = "relay-c".to_string();
    regular_settings.active_aggregate_relay_id.clear();
    let regular = select_relay_for_request(&regular_settings, RotationContext::default()).unwrap();
    let after_reselect = select_relay_for_request(&settings, RotationContext::default()).unwrap();

    assert_eq!(first.id, "relay-a");
    assert_eq!(regular.id, "relay-c");
    assert_eq!(after_reselect.id, "relay-a");
}

#[test]
fn record_relay_request_failure_advances_global_failover_selector() {
    let _guard = global_selector_test_lock();
    let aggregate_id = "agg-global-failure";
    let settings = BackendSettings {
        relay_profiles: vec![
            profile("relay-a"),
            profile("relay-b"),
            RelayProfile {
                id: aggregate_id.to_string(),
                name: "聚合".to_string(),
                relay_mode: RelayMode::Aggregate,
                ..RelayProfile::default()
            },
        ],
        aggregate_relay_profiles: vec![aggregate_with_id(
            aggregate_id,
            AggregateRelayStrategy::Failover,
        )],
        active_relay_id: aggregate_id.to_string(),
        active_aggregate_relay_id: aggregate_id.to_string(),
        ..BackendSettings::default()
    };

    let first = select_relay_for_request(&settings, RotationContext::default()).unwrap();
    record_relay_request_failure(&settings);
    let second = select_relay_for_request(&settings, RotationContext::default()).unwrap();

    assert_eq!(first.id, "relay-a");
    assert_eq!(second.id, "relay-b");
}

#[test]
fn global_priority_fallback_selector_skips_a_after_429() {
    let _guard = global_selector_test_lock();
    let aggregate_id = "agg-global-priority-fallback";
    let settings = BackendSettings {
        relay_profiles: vec![
            profile("relay-a"),
            profile("relay-b"),
            RelayProfile {
                id: aggregate_id.to_string(),
                name: "聚合".to_string(),
                relay_mode: RelayMode::Aggregate,
                ..RelayProfile::default()
            },
        ],
        aggregate_relay_profiles: vec![aggregate_with_id(
            aggregate_id,
            AggregateRelayStrategy::PriorityFallback,
        )],
        active_relay_id: aggregate_id.to_string(),
        active_aggregate_relay_id: aggregate_id.to_string(),
        ..BackendSettings::default()
    };

    let first = select_relay_for_request(&settings, RotationContext::default()).unwrap();
    for _ in 0..3 {
        record_relay_request_outcome(&settings, &first.id, RelayRequestOutcome::HttpStatus(429));
    }
    let second = select_relay_for_request(&settings, RotationContext::default()).unwrap();

    assert_eq!(first.id, "relay-a");
    assert_eq!(second.id, "relay-b");

    let reset = reset_priority_fallback_cooldowns(&settings);
    assert!(
        reset
            .members
            .iter()
            .all(|member| member.cooldown_remaining_seconds == 0)
    );
    let after_reset = select_relay_for_request(&settings, RotationContext::default()).unwrap();
    assert_eq!(after_reset.id, "relay-a");
}

#[test]
fn select_relay_for_probe_does_not_advance_request_rotation() {
    let _guard = global_selector_test_lock();
    let aggregate_id = "agg-probe";
    let settings = BackendSettings {
        relay_profiles: vec![
            profile("relay-a"),
            profile("relay-b"),
            RelayProfile {
                id: aggregate_id.to_string(),
                name: "聚合".to_string(),
                relay_mode: RelayMode::Aggregate,
                ..RelayProfile::default()
            },
        ],
        aggregate_relay_profiles: vec![aggregate_with_id(
            aggregate_id,
            AggregateRelayStrategy::RequestRoundRobin,
        )],
        active_relay_id: aggregate_id.to_string(),
        active_aggregate_relay_id: aggregate_id.to_string(),
        ..BackendSettings::default()
    };

    let first_probe = select_relay_for_probe(&settings).unwrap();
    let second_probe = select_relay_for_probe(&settings).unwrap();
    let first_request = select_relay_for_request(&settings, RotationContext::default()).unwrap();
    let second_request = select_relay_for_request(&settings, RotationContext::default()).unwrap();

    assert_eq!(first_probe.id, "relay-a");
    assert_eq!(second_probe.id, "relay-a");
    assert_eq!(first_request.id, "relay-a");
    assert_eq!(second_request.id, "relay-b");
}

#[test]
fn fallback_relays_after_returns_remaining_aggregate_members_after_current_then_wraps() {
    let settings = settings(AggregateRelayStrategy::RequestRoundRobin);

    let fallbacks = fallback_relays_after(&settings, "relay-b").unwrap();

    assert_eq!(
        fallbacks
            .iter()
            .map(|profile| profile.id.as_str())
            .collect::<Vec<_>>(),
        vec!["relay-c", "relay-a"]
    );
}

#[test]
fn fallback_relays_after_regular_relay_returns_empty_candidates() {
    let mut settings = settings(AggregateRelayStrategy::RequestRoundRobin);
    settings.active_relay_id = "relay-a".to_string();

    let fallbacks = fallback_relays_after(&settings, "relay-a").unwrap();

    assert!(fallbacks.is_empty());
}

#[test]
fn select_relay_for_request_rebuilds_selector_when_active_aggregate_changes() {
    let _guard = global_selector_test_lock();
    let aggregate_id = "agg-refresh";
    let mut settings = BackendSettings {
        relay_profiles: vec![
            profile("relay-a"),
            profile("relay-b"),
            RelayProfile {
                id: aggregate_id.to_string(),
                name: "聚合".to_string(),
                relay_mode: RelayMode::Aggregate,
                ..RelayProfile::default()
            },
        ],
        aggregate_relay_profiles: vec![aggregate_with_id(
            aggregate_id,
            AggregateRelayStrategy::Failover,
        )],
        active_relay_id: aggregate_id.to_string(),
        active_aggregate_relay_id: aggregate_id.to_string(),
        ..BackendSettings::default()
    };

    let first = select_relay_for_request(&settings, RotationContext::default()).unwrap();
    settings.aggregate_relay_profiles[0].strategy = AggregateRelayStrategy::WeightedRoundRobin;

    let selected = (0..3)
        .map(|_| {
            select_relay_for_request(&settings, RotationContext::default())
                .unwrap()
                .id
        })
        .collect::<Vec<_>>();

    assert_eq!(first.id, "relay-a");
    assert_eq!(selected, vec!["relay-a", "relay-b", "relay-b"]);
}
