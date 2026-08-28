/**
 * @description 聚合供应商轮转选择器，负责按失败、对话、请求和权重策略选择已有中转配置。
 * @author Albert_Luo
 * @email 480199976@qq.com
 * @date 2026-05-27 00:00
 */
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::settings::{
    AggregateRelayProfile, AggregateRelayStrategy, BackendSettings, RelayProfile,
};

pub const PRIORITY_FALLBACK_FAILURE_THRESHOLD: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionError {
    NoActiveAggregate,
    EmptyAggregateMembers {
        aggregate_id: String,
    },
    UnknownMemberRelay {
        aggregate_id: String,
        relay_id: String,
    },
    InvalidMemberRelay {
        aggregate_id: String,
        relay_id: String,
    },
}

impl std::fmt::Display for SelectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SelectionError::NoActiveAggregate => write!(formatter, "未找到当前聚合供应商"),
            SelectionError::EmptyAggregateMembers { aggregate_id } => {
                write!(formatter, "聚合供应商「{aggregate_id}」没有成员")
            }
            SelectionError::UnknownMemberRelay {
                aggregate_id,
                relay_id,
            } => write!(
                formatter,
                "聚合供应商「{aggregate_id}」引用了不存在的供应商「{relay_id}」"
            ),
            SelectionError::InvalidMemberRelay {
                aggregate_id,
                relay_id,
            } => write!(
                formatter,
                "聚合供应商「{aggregate_id}」成员「{relay_id}」缺少 API Base URL 或 Key"
            ),
        }
    }
}

impl std::error::Error for SelectionError {}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RotationContext {
    pub conversation_id: Option<String>,
}

impl RotationContext {
    pub fn for_conversation(conversation_id: impl Into<String>) -> Self {
        Self {
            conversation_id: Some(conversation_id.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationEvent {
    Success,
    Failure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "statusCode", rename_all = "camelCase")]
pub enum RelayRequestOutcome {
    HttpStatus(u16),
    TransportFailure,
}

impl RelayRequestOutcome {
    pub fn cooldown_duration(self) -> Option<Duration> {
        match self {
            Self::HttpStatus(status) if (200..300).contains(&status) => None,
            Self::HttpStatus(429) => Some(Duration::from_secs(30 * 60)),
            Self::HttpStatus(402) => Some(Duration::from_secs(60 * 60)),
            Self::HttpStatus(502) | Self::HttpStatus(503) => Some(Duration::from_secs(3 * 60)),
            Self::HttpStatus(_) | Self::TransportFailure => Some(Duration::from_secs(10 * 60)),
        }
    }

    fn rotation_event(self) -> RotationEvent {
        if self.cooldown_duration().is_some() {
            RotationEvent::Failure
        } else {
            RotationEvent::Success
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayCooldownMemberStatus {
    pub relay_id: String,
    pub consecutive_failures: u32,
    pub failure_threshold: u32,
    pub cooldown_remaining_seconds: u64,
    pub last_cooldown_reason: Option<RelayRequestOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayCooldownStatus {
    pub aggregate_id: Option<String>,
    pub members: Vec<RelayCooldownMemberStatus>,
}

#[derive(Debug, Clone)]
pub struct RelayRotationSelector {
    aggregate: AggregateRelayProfile,
    priority_member_profiles: Option<Vec<RelayProfile>>,
    failover_index: usize,
    request_index: usize,
    weighted_index: usize,
    conversation_assignments: HashMap<String, String>,
    cooldowns: HashMap<String, Instant>,
    consecutive_failures: HashMap<String, u32>,
    last_cooldown_reasons: HashMap<String, RelayRequestOutcome>,
}

static GLOBAL_SELECTOR: OnceLock<Mutex<Option<RelayRotationSelector>>> = OnceLock::new();

impl RelayRotationSelector {
    pub fn from_settings(settings: &BackendSettings) -> Result<Self, SelectionError> {
        let aggregate = active_aggregate(settings)?.clone();
        validate_aggregate_members(settings, &aggregate)?;
        let priority_member_profiles =
            (aggregate.strategy == AggregateRelayStrategy::PriorityFallback).then(|| {
                aggregate
                    .members
                    .iter()
                    .filter_map(|member| relay_profile_by_id(settings, &member.relay_id))
                    .collect()
            });
        Ok(Self {
            aggregate,
            priority_member_profiles,
            failover_index: 0,
            request_index: 0,
            weighted_index: 0,
            conversation_assignments: HashMap::new(),
            cooldowns: HashMap::new(),
            consecutive_failures: HashMap::new(),
            last_cooldown_reasons: HashMap::new(),
        })
    }

    pub fn select(
        &mut self,
        settings: &BackendSettings,
        context: RotationContext,
    ) -> Result<RelayProfile, SelectionError> {
        self.select_at(settings, context, Instant::now())
    }

    pub fn select_at(
        &mut self,
        settings: &BackendSettings,
        context: RotationContext,
        now: Instant,
    ) -> Result<RelayProfile, SelectionError> {
        validate_aggregate_members(settings, &self.aggregate)?;
        self.prune_expired_cooldowns(now);
        let relay_id = match self.aggregate.strategy {
            AggregateRelayStrategy::Failover => self.member_id_at(self.failover_index),
            AggregateRelayStrategy::PriorityFallback => self.select_priority_fallback(now),
            AggregateRelayStrategy::ConversationRoundRobin => {
                self.select_for_conversation(context.conversation_id)
            }
            AggregateRelayStrategy::RequestRoundRobin => self.select_next_request(),
            AggregateRelayStrategy::WeightedRoundRobin => self.select_next_weighted(),
        };
        relay_profile_by_id(settings, &relay_id).ok_or_else(|| SelectionError::UnknownMemberRelay {
            aggregate_id: self.aggregate.id.clone(),
            relay_id,
        })
    }

    pub fn peek(&self, settings: &BackendSettings) -> Result<RelayProfile, SelectionError> {
        self.peek_at(settings, Instant::now())
    }

    pub fn peek_at(
        &self,
        settings: &BackendSettings,
        now: Instant,
    ) -> Result<RelayProfile, SelectionError> {
        validate_aggregate_members(settings, &self.aggregate)?;
        let relay_id = match self.aggregate.strategy {
            AggregateRelayStrategy::Failover => self.member_id_at(self.failover_index),
            AggregateRelayStrategy::PriorityFallback => self.select_priority_fallback(now),
            AggregateRelayStrategy::ConversationRoundRobin
            | AggregateRelayStrategy::RequestRoundRobin => self.member_id_at(self.request_index),
            AggregateRelayStrategy::WeightedRoundRobin => {
                let schedule = self.weighted_schedule();
                schedule[self.weighted_index % schedule.len()].clone()
            }
        };
        relay_profile_by_id(settings, &relay_id).ok_or_else(|| SelectionError::UnknownMemberRelay {
            aggregate_id: self.aggregate.id.clone(),
            relay_id,
        })
    }

    pub fn record_event(&mut self, event: RotationEvent) {
        if event == RotationEvent::Failure
            && self.aggregate.strategy == AggregateRelayStrategy::Failover
            && !self.aggregate.members.is_empty()
        {
            self.failover_index = (self.failover_index + 1) % self.aggregate.members.len();
        }
    }

    pub fn record_outcome_at(
        &mut self,
        relay_id: &str,
        outcome: RelayRequestOutcome,
        now: Instant,
    ) -> bool {
        if self.aggregate.strategy != AggregateRelayStrategy::PriorityFallback {
            self.record_event(outcome.rotation_event());
            return outcome.cooldown_duration().is_some();
        }
        if !self
            .aggregate
            .members
            .iter()
            .any(|member| member.relay_id == relay_id)
        {
            return false;
        }
        if outcome.cooldown_duration().is_none() {
            self.cooldowns.remove(relay_id);
            self.consecutive_failures.remove(relay_id);
            return false;
        }
        if self
            .cooldowns
            .get(relay_id)
            .is_some_and(|cooldown_until| *cooldown_until > now)
        {
            return false;
        }
        let failures = self
            .consecutive_failures
            .entry(relay_id.to_string())
            .or_default();
        *failures += 1;
        if *failures >= PRIORITY_FALLBACK_FAILURE_THRESHOLD {
            let duration = outcome
                .cooldown_duration()
                .expect("failure outcome has a cooldown duration");
            self.cooldowns.insert(relay_id.to_string(), now + duration);
            self.last_cooldown_reasons
                .insert(relay_id.to_string(), outcome);
            self.consecutive_failures.remove(relay_id);
            return true;
        }
        false
    }

    pub fn cooldown_until(&self, relay_id: &str) -> Option<Instant> {
        self.cooldowns.get(relay_id).copied()
    }

    fn matches_settings(
        &self,
        settings: &BackendSettings,
        active_aggregate: &AggregateRelayProfile,
    ) -> bool {
        if self.aggregate != *active_aggregate {
            return false;
        }
        let Some(previous_members) = &self.priority_member_profiles else {
            return true;
        };
        let current_members = active_aggregate
            .members
            .iter()
            .filter_map(|member| relay_profile_by_id(settings, &member.relay_id))
            .collect::<Vec<_>>();
        *previous_members == current_members
    }

    pub fn consecutive_failures(&self, relay_id: &str) -> u32 {
        self.consecutive_failures
            .get(relay_id)
            .copied()
            .unwrap_or_default()
    }

    pub fn cooldown_status_at(&mut self, now: Instant) -> RelayCooldownStatus {
        self.prune_expired_cooldowns(now);
        RelayCooldownStatus {
            aggregate_id: Some(self.aggregate.id.clone()),
            members: self
                .aggregate
                .members
                .iter()
                .map(|member| RelayCooldownMemberStatus {
                    relay_id: member.relay_id.clone(),
                    consecutive_failures: self.consecutive_failures(&member.relay_id),
                    failure_threshold: PRIORITY_FALLBACK_FAILURE_THRESHOLD,
                    cooldown_remaining_seconds: self
                        .cooldowns
                        .get(&member.relay_id)
                        .map(|until| {
                            until
                                .saturating_duration_since(now)
                                .as_millis()
                                .div_ceil(1000) as u64
                        })
                        .unwrap_or_default(),
                    last_cooldown_reason: self.last_cooldown_reasons.get(&member.relay_id).copied(),
                })
                .collect(),
        }
    }

    pub fn reset_priority_fallback_cooldowns(&mut self) {
        self.cooldowns.clear();
        self.consecutive_failures.clear();
    }

    pub fn priority_fallbacks_after(&self, relay_id: &str, now: Instant) -> Vec<String> {
        let Some(start_index) = self
            .aggregate
            .members
            .iter()
            .position(|member| member.relay_id == relay_id)
        else {
            return Vec::new();
        };
        if self
            .aggregate
            .members
            .iter()
            .all(|member| !self.is_available(&member.relay_id, now))
        {
            // 所有成员都在冷却时，不再返回其它冷却中的成员作为后备，
            // 否则协议代理会逐个尝试它们并把冷却时间无限顺延，造成无限重试。
            // 此时只保留 select 选出的“最早到期”成员作为唯一候选，失败即报错。
            return Vec::new();
        }
        self.aggregate
            .members
            .iter()
            .skip(start_index + 1)
            .filter(|member| self.is_available(&member.relay_id, now))
            .map(|member| member.relay_id.clone())
            .collect()
    }

    fn select_priority_fallback(&self, now: Instant) -> String {
        if let Some(member) = self
            .aggregate
            .members
            .iter()
            .find(|member| self.is_available(&member.relay_id, now))
        {
            return member.relay_id.clone();
        }
        self.aggregate
            .members
            .iter()
            .min_by_key(|member| self.cooldowns.get(&member.relay_id).copied())
            .expect("aggregate members validated as non-empty")
            .relay_id
            .clone()
    }

    fn is_available(&self, relay_id: &str, now: Instant) -> bool {
        self.cooldowns
            .get(relay_id)
            .is_none_or(|cooldown_until| *cooldown_until <= now)
    }

    fn prune_expired_cooldowns(&mut self, now: Instant) {
        self.cooldowns
            .retain(|_, cooldown_until| *cooldown_until > now);
    }

    fn select_for_conversation(&mut self, conversation_id: Option<String>) -> String {
        let Some(conversation_id) = conversation_id else {
            return self.select_next_request();
        };
        if let Some(relay_id) = self.conversation_assignments.get(&conversation_id) {
            return relay_id.clone();
        }

        let relay_id = self.select_next_request();
        self.conversation_assignments
            .insert(conversation_id, relay_id.clone());
        relay_id
    }

    fn select_next_request(&mut self) -> String {
        let relay_id = self.member_id_at(self.request_index);
        self.request_index = (self.request_index + 1) % self.aggregate.members.len();
        relay_id
    }

    fn select_next_weighted(&mut self) -> String {
        let schedule = self.weighted_schedule();
        let relay_id = schedule[self.weighted_index % schedule.len()].clone();
        self.weighted_index = (self.weighted_index + 1) % schedule.len();
        relay_id
    }

    fn weighted_schedule(&self) -> Vec<String> {
        self.aggregate
            .members
            .iter()
            .flat_map(|member| {
                std::iter::repeat_n(member.relay_id.clone(), member.weight.max(1) as usize)
            })
            .collect()
    }

    fn member_id_at(&self, index: usize) -> String {
        self.aggregate.members[index % self.aggregate.members.len()]
            .relay_id
            .clone()
    }
}

pub fn select_relay_for_request(
    settings: &BackendSettings,
    context: RotationContext,
) -> Result<RelayProfile, SelectionError> {
    let Some(active_aggregate) = settings.active_aggregate_relay_profile() else {
        clear_global_selector();
        return Ok(settings.active_relay_profile());
    };

    let lock = GLOBAL_SELECTOR.get_or_init(|| Mutex::new(None));
    let mut guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let needs_new_selector = guard
        .as_ref()
        .is_none_or(|selector| !selector.matches_settings(settings, &active_aggregate));
    if needs_new_selector {
        *guard = Some(RelayRotationSelector::from_settings(settings)?);
    }
    guard
        .as_mut()
        .expect("selector initialized")
        .select(settings, context)
}

pub fn select_relay_for_probe(settings: &BackendSettings) -> Result<RelayProfile, SelectionError> {
    let Some(active_aggregate) = settings.active_aggregate_relay_profile() else {
        clear_global_selector();
        return Ok(settings.active_relay_profile());
    };

    let lock = GLOBAL_SELECTOR.get_or_init(|| Mutex::new(None));
    let mut guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let needs_new_selector = guard
        .as_ref()
        .is_none_or(|selector| !selector.matches_settings(settings, &active_aggregate));
    if needs_new_selector {
        *guard = Some(RelayRotationSelector::from_settings(settings)?);
    }
    guard.as_ref().expect("selector initialized").peek(settings)
}

pub fn fallback_relays_after(
    settings: &BackendSettings,
    relay_id: &str,
) -> Result<Vec<RelayProfile>, SelectionError> {
    fallback_relays_after_at(settings, relay_id, Instant::now())
}

pub fn fallback_relays_after_at(
    settings: &BackendSettings,
    relay_id: &str,
    now: Instant,
) -> Result<Vec<RelayProfile>, SelectionError> {
    let Some(active_aggregate) = settings.active_aggregate_relay_profile() else {
        return Ok(Vec::new());
    };
    validate_aggregate_members(settings, &active_aggregate)?;
    let relay_ids = if active_aggregate.strategy == AggregateRelayStrategy::PriorityFallback {
        let lock = GLOBAL_SELECTOR.get_or_init(|| Mutex::new(None));
        let mut guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let needs_new_selector = guard
            .as_ref()
            .is_none_or(|selector| !selector.matches_settings(settings, &active_aggregate));
        if needs_new_selector {
            *guard = Some(RelayRotationSelector::from_settings(settings)?);
        }
        guard
            .as_ref()
            .expect("selector initialized")
            .priority_fallbacks_after(relay_id, now)
    } else {
        let start_index = active_aggregate
            .members
            .iter()
            .position(|member| member.relay_id == relay_id)
            .map(|index| index + 1)
            .unwrap_or(0);
        (0..active_aggregate.members.len().saturating_sub(1))
            .map(|offset| {
                let index = (start_index + offset) % active_aggregate.members.len();
                active_aggregate.members[index].relay_id.clone()
            })
            .collect()
    };
    relay_ids
        .into_iter()
        .map(|candidate_id| {
            relay_profile_by_id(settings, &candidate_id).ok_or_else(|| {
                SelectionError::UnknownMemberRelay {
                    aggregate_id: active_aggregate.id.clone(),
                    relay_id: candidate_id,
                }
            })
        })
        .collect()
}

pub fn record_relay_request_outcome(
    settings: &BackendSettings,
    relay_id: &str,
    outcome: RelayRequestOutcome,
) -> bool {
    record_relay_request_outcome_at(settings, relay_id, outcome, Instant::now())
}

pub fn record_relay_request_outcome_at(
    settings: &BackendSettings,
    relay_id: &str,
    outcome: RelayRequestOutcome,
    now: Instant,
) -> bool {
    let Some(active_aggregate) = settings.active_aggregate_relay_profile() else {
        clear_global_selector();
        return false;
    };
    let lock = GLOBAL_SELECTOR.get_or_init(|| Mutex::new(None));
    let mut guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let needs_new_selector = guard
        .as_ref()
        .is_none_or(|selector| !selector.matches_settings(settings, &active_aggregate));
    if needs_new_selector {
        let Ok(selector) = RelayRotationSelector::from_settings(settings) else {
            return false;
        };
        *guard = Some(selector);
    }
    if let Some(selector) = guard.as_mut() {
        return selector.record_outcome_at(relay_id, outcome, now);
    }
    false
}

pub fn record_relay_request_event(settings: &BackendSettings, event: RotationEvent) {
    if settings.active_aggregate_relay_profile().is_none() {
        clear_global_selector();
        return;
    }
    let lock = GLOBAL_SELECTOR.get_or_init(|| Mutex::new(None));
    let mut guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(selector) = guard.as_mut() {
        selector.record_event(event);
    }
}

pub fn record_relay_request_failure(settings: &BackendSettings) {
    record_relay_request_event(settings, RotationEvent::Failure);
}

pub fn priority_fallback_cooldown_status(settings: &BackendSettings) -> RelayCooldownStatus {
    priority_fallback_cooldown_status_at(settings, Instant::now())
}

pub fn reset_priority_fallback_cooldowns(settings: &BackendSettings) -> RelayCooldownStatus {
    let status = with_priority_fallback_selector(settings, |selector| {
        selector.reset_priority_fallback_cooldowns();
        selector.cooldown_status_at(Instant::now())
    });
    status.unwrap_or_else(empty_cooldown_status)
}

pub fn reset_all_priority_fallback_cooldowns() -> RelayCooldownStatus {
    let lock = GLOBAL_SELECTOR.get_or_init(|| Mutex::new(None));
    let mut guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(selector) = guard.as_mut() else {
        return empty_cooldown_status();
    };
    selector.reset_priority_fallback_cooldowns();
    selector.cooldown_status_at(Instant::now())
}

fn priority_fallback_cooldown_status_at(
    settings: &BackendSettings,
    now: Instant,
) -> RelayCooldownStatus {
    with_priority_fallback_selector(settings, |selector| selector.cooldown_status_at(now))
        .unwrap_or_else(empty_cooldown_status)
}

fn with_priority_fallback_selector<T>(
    settings: &BackendSettings,
    operation: impl FnOnce(&mut RelayRotationSelector) -> T,
) -> Option<T> {
    let active_aggregate = settings.active_aggregate_relay_profile()?;
    if active_aggregate.strategy != AggregateRelayStrategy::PriorityFallback {
        return None;
    }
    let lock = GLOBAL_SELECTOR.get_or_init(|| Mutex::new(None));
    let mut guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let needs_new_selector = guard
        .as_ref()
        .is_none_or(|selector| !selector.matches_settings(settings, &active_aggregate));
    if needs_new_selector {
        *guard = RelayRotationSelector::from_settings(settings).ok();
    }
    guard.as_mut().map(operation)
}

fn empty_cooldown_status() -> RelayCooldownStatus {
    RelayCooldownStatus {
        aggregate_id: None,
        members: Vec::new(),
    }
}

fn active_aggregate(settings: &BackendSettings) -> Result<&AggregateRelayProfile, SelectionError> {
    let active_id = settings
        .active_aggregate_relay_profile()
        .map(|aggregate| aggregate.id)
        .ok_or(SelectionError::NoActiveAggregate)?;

    settings
        .aggregate_relay_profiles
        .iter()
        .find(|aggregate| aggregate.id == active_id)
        .ok_or(SelectionError::NoActiveAggregate)
}

fn validate_aggregate_members(
    settings: &BackendSettings,
    aggregate: &AggregateRelayProfile,
) -> Result<(), SelectionError> {
    if aggregate.members.is_empty() {
        return Err(SelectionError::EmptyAggregateMembers {
            aggregate_id: aggregate.id.clone(),
        });
    }

    let relay_by_id = settings
        .relay_profiles
        .iter()
        .map(|profile| (profile.id.as_str(), profile))
        .collect::<HashMap<_, _>>();
    for member in &aggregate.members {
        let Some(relay) = relay_by_id.get(member.relay_id.as_str()) else {
            return Err(SelectionError::UnknownMemberRelay {
                aggregate_id: aggregate.id.clone(),
                relay_id: member.relay_id.clone(),
            });
        };
        if relay.base_url.trim().is_empty()
            || (relay.api_key.trim().is_empty() && !relay.uses_no_auth())
        {
            return Err(SelectionError::InvalidMemberRelay {
                aggregate_id: aggregate.id.clone(),
                relay_id: member.relay_id.clone(),
            });
        }
    }
    Ok(())
}

fn clear_global_selector() {
    let lock = GLOBAL_SELECTOR.get_or_init(|| Mutex::new(None));
    let mut guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = None;
}

fn relay_profile_by_id(settings: &BackendSettings, relay_id: &str) -> Option<RelayProfile> {
    settings
        .relay_profiles
        .iter()
        .find(|profile| profile.id == relay_id)
        .cloned()
}
