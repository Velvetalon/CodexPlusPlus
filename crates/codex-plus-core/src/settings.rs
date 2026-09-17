use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;
use serde_json::{Map, Value};
use toml_edit::{DocumentMut, Item};

use crate::zed_remote::ZedOpenStrategy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LaunchMode {
    #[default]
    Patch,
    Relay,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayProfile {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing)]
    pub model: String,
    #[serde(default = "default_relay_base_url", skip_serializing)]
    pub base_url: String,
    #[serde(rename = "upstreamBaseUrl", default)]
    pub upstream_base_url: String,
    #[serde(
        default,
        skip_serializing,
        deserialize_with = "deserialize_profile_api_key"
    )]
    pub api_key: String,
    #[serde(default)]
    pub protocol: RelayProtocol,
    #[serde(rename = "responsesReasoningPolicy", default)]
    pub responses_reasoning_policy: ResponsesReasoningPolicy,
    #[serde(rename = "responsesWirePolicy", default)]
    pub responses_wire_policy: ResponsesWirePolicy,
    /// Custom-as-Function 双向适配开关（默认关闭）。缺失字段的旧 profile 反序列化
    /// 为 false，原有行为完全不变；始终序列化，保证 CLI provider-update 的
    /// 允许字段检查能看到它。
    #[serde(rename = "customToolsAsFunctions", default)]
    pub custom_tools_as_functions: bool,
    #[serde(rename = "nativeAgentInterop", default)]
    pub native_agent_interop: NativeAgentInterop,
    #[serde(rename = "relayMode", default)]
    pub relay_mode: RelayMode,
    #[serde(rename = "officialMixApiKey", default)]
    pub official_mix_api_key: bool,
    #[serde(rename = "noAuth", default)]
    pub no_auth: bool,
    #[serde(rename = "hideOfficialUsageAlert", default)]
    pub hide_official_usage_alert: bool,
    #[serde(rename = "testModel", default)]
    pub test_model: String,
    #[serde(rename = "configContents", default)]
    pub config_contents: String,
    #[serde(rename = "authContents", default)]
    pub auth_contents: String,
    #[serde(rename = "useCommonConfig", default = "default_true")]
    pub use_common_config: bool,
    #[serde(rename = "contextWindow", default)]
    pub context_window: String,
    #[serde(rename = "autoCompactLimit", default)]
    pub auto_compact_limit: String,
    #[serde(default)]
    pub new_context_management: bool,
    #[serde(rename = "modelInsertMode", default)]
    pub model_insert_mode: RelayModelInsertMode,
    #[serde(rename = "modelList", default)]
    pub model_list: String,
    #[serde(
        rename = "modelWindows",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub model_windows: String,
    #[serde(rename = "modelVlm", default, skip_serializing_if = "String::is_empty")]
    pub model_vlm: String,
    #[serde(
        rename = "vlmApiKey",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub vlm_api_key: String,
    #[serde(rename = "vlmModel", default)]
    pub vlm_model: String,
    #[serde(rename = "vlmBaseUrl", default)]
    pub vlm_base_url: String,
    #[serde(
        rename = "userAgent",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub user_agent: String,
    #[serde(rename = "sub2apiEnabled", default)]
    pub sub2api_enabled: bool,
    #[serde(
        rename = "sub2apiMultiplier",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub sub2api_multiplier: String,
    #[serde(rename = "modelRoutes", default, skip_serializing_if = "Vec::is_empty")]
    pub model_routes: Vec<RelayModelRoute>,
    #[serde(
        rename = "modelAliases",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub model_aliases: Vec<RelayModelAlias>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayModelRoute {
    pub model: String,
    #[serde(rename = "targetRelayId")]
    pub target_relay_id: String,
    #[serde(
        rename = "targetModel",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub target_model: String,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    #[serde(rename = "restoreAt", default, skip_serializing_if = "Option::is_none")]
    pub restore_at: Option<u64>,
}

impl RelayModelRoute {
    pub fn is_effectively_enabled_at(&self, now_ms: u64) -> bool {
        self.enabled
            || self
                .restore_at
                .is_some_and(|restore_at| restore_at <= now_ms)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayModelRouteStatus {
    pub model: String,
    pub target_relay_id: String,
    pub target_relay_name: String,
    pub target_model: String,
    pub enabled: bool,
    pub restore_at: Option<u64>,
    pub permanent: bool,
    pub remaining_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayModelRoutesResult {
    pub status: &'static str,
    pub provider_id: String,
    pub provider_name: String,
    pub observed_at: u64,
    pub routes: Vec<RelayModelRouteStatus>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetRelayModelRouteRequest {
    pub id: String,
    pub model: String,
    pub enabled: bool,
    #[serde(default)]
    pub restore_at: Option<u64>,
    #[serde(default)]
    pub duration_seconds: Option<u64>,
    #[serde(default)]
    pub permanent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetRelayModelRouteResult {
    pub status: &'static str,
    pub provider_id: String,
    pub observed_at: u64,
    pub route: RelayModelRouteStatus,
    pub dry_run: bool,
    pub restart_requested: bool,
    pub applied_live_config: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum AggregateRelayStrategy {
    #[default]
    Failover,
    PriorityFallback,
    ConversationRoundRobin,
    RequestRoundRobin,
    WeightedRoundRobin,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregateRelayMember {
    #[serde(rename = "relayId")]
    pub relay_id: String,
    #[serde(default = "default_aggregate_member_weight")]
    pub weight: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RelaySessionProvider {
    #[default]
    Custom,
    Openai,
}

impl RelaySessionProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Custom => "custom",
            Self::Openai => "openai",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregateRelayProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub session_provider: RelaySessionProvider,
    #[serde(default)]
    pub code_mode_host: bool,
    #[serde(default)]
    pub strategy: AggregateRelayStrategy,
    #[serde(default)]
    pub members: Vec<AggregateRelayMember>,
}

impl Default for RelayProfile {
    fn default() -> Self {
        Self {
            id: "default".to_string(),
            name: "默认中转".to_string(),
            model: String::new(),
            base_url: default_relay_base_url(),
            upstream_base_url: String::new(),
            api_key: String::new(),
            protocol: RelayProtocol::Responses,
            responses_reasoning_policy: ResponsesReasoningPolicy::default(),
            responses_wire_policy: ResponsesWirePolicy::default(),
            custom_tools_as_functions: false,
            native_agent_interop: NativeAgentInterop::default(),
            relay_mode: RelayMode::Official,
            official_mix_api_key: false,
            no_auth: false,
            hide_official_usage_alert: false,
            test_model: String::new(),
            config_contents: String::new(),
            auth_contents: String::new(),
            use_common_config: true,
            context_window: String::new(),
            auto_compact_limit: String::new(),
            new_context_management: false,
            model_insert_mode: RelayModelInsertMode::Patch,
            model_list: String::new(),
            model_windows: String::new(),
            model_vlm: String::new(),
            vlm_api_key: String::new(),
            vlm_model: String::new(),
            vlm_base_url: String::new(),
            user_agent: String::new(),
            sub2api_enabled: false,
            sub2api_multiplier: String::new(),
            model_routes: Vec::new(),
            model_aliases: Vec::new(),
        }
    }
}

impl RelayProfile {
    pub fn uses_no_auth(&self) -> bool {
        self.relay_mode == RelayMode::PureApi && self.no_auth
    }

    pub fn has_model_routes(&self) -> bool {
        self.model_routes
            .iter()
            .any(|route| !route.model.trim().is_empty() && !route.target_relay_id.trim().is_empty())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum RelayModelInsertMode {
    ModelCatalog,
    #[default]
    Patch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum RelayProtocol {
    #[default]
    Responses,
    ChatCompletions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ResponsesReasoningPolicy {
    #[default]
    Passthrough,
    OpenAiOpaque,
    Strip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum NativeAgentInterop {
    #[default]
    Auto,
    On,
    Off,
}

impl NativeAgentInterop {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
        }
    }
}

/// R08/R07.2：Responses wire 结构策略。
/// compatible：沿用既有兼容能力（namespace 扁平化、ID 规范化、原生子任务别名等）；
/// passthrough：跳过全部 Codex 扩展/工具/任务/ID 结构改写，供原生透传链路使用。
/// 缺失该字段的旧 profile 反序列化时取 Compatible，保持既有行为不变。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ResponsesWirePolicy {
    #[default]
    Compatible,
    Passthrough,
}

impl ResponsesWirePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compatible => "compatible",
            Self::Passthrough => "passthrough",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayModelAlias {
    pub alias: String,
    pub model: String,
}

impl ResponsesReasoningPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passthrough => "passthrough",
            Self::OpenAiOpaque => "openAiOpaque",
            Self::Strip => "strip",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum RelayMode {
    Official,
    #[default]
    MixedApi,
    PureApi,
    Aggregate,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DreamSkinColors {
    pub background: String,
    pub panel: String,
    pub panel_alt: String,
    pub accent: String,
    pub accent_alt: String,
    pub secondary: String,
    pub highlight: String,
    pub text: String,
    pub muted: String,
    pub line: String,
}

impl Default for DreamSkinColors {
    fn default() -> Self {
        Self {
            background: "#F7F4F5".to_string(),
            panel: "#FFFFFF".to_string(),
            panel_alt: "#FFF7F8".to_string(),
            accent: "#E25563".to_string(),
            accent_alt: "#F07A86".to_string(),
            secondary: "#F3A8AF".to_string(),
            highlight: "#C93D4C".to_string(),
            text: "#2B2224".to_string(),
            muted: "#8A7A7D".to_string(),
            line: "rgba(196, 120, 128, .22)".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DreamSkinThemeConfig {
    #[serde(default = "default_dream_skin_schema_version")]
    pub schema_version: u8,
    #[serde(default = "default_dream_skin_id")]
    pub id: String,
    #[serde(default = "default_dream_skin_name")]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub style_preset: String,
    #[serde(default = "default_dream_skin_brand_subtitle")]
    pub brand_subtitle: String,
    #[serde(default = "default_dream_skin_tagline")]
    pub tagline: String,
    #[serde(default = "default_dream_skin_project_prefix")]
    pub project_prefix: String,
    #[serde(default = "default_dream_skin_project_label")]
    pub project_label: String,
    #[serde(default = "default_dream_skin_status_text")]
    pub status_text: String,
    #[serde(default = "default_dream_skin_quote")]
    pub quote: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colors: Option<DreamSkinColors>,
    #[serde(flatten)]
    pub extra_fields: Map<String, Value>,
}

impl Default for DreamSkinThemeConfig {
    fn default() -> Self {
        let mut extra_fields = Map::new();
        #[cfg(windows)]
        {
            extra_fields.insert(
                "image".to_string(),
                Value::String("dream-reference.jpg".to_string()),
            );
            extra_fields.insert("appearance".to_string(), Value::String("auto".to_string()));
            extra_fields.insert(
                "art".to_string(),
                serde_json::json!({
                    "focusX": 0.72,
                    "focusY": 0.45,
                    "safeArea": "left",
                    "taskMode": "ambient"
                }),
            );
        }
        #[cfg(not(windows))]
        {
            extra_fields.insert(
                "image".to_string(),
                Value::String("portal-hero.png".to_string()),
            );
            extra_fields.insert(
                "promoTitle".to_string(),
                Value::String("感谢 Passion8 赞助".to_string()),
            );
            extra_fields.insert(
                "promoSub".to_string(),
                Value::String("passion8.cc".to_string()),
            );
            extra_fields.insert(
                "promoUrl".to_string(),
                Value::String("https://passion8.cc/register?aff=TuPe".to_string()),
            );
        }
        Self {
            schema_version: default_dream_skin_schema_version(),
            id: default_dream_skin_id(),
            name: default_dream_skin_name(),
            style_preset: String::new(),
            brand_subtitle: default_dream_skin_brand_subtitle(),
            tagline: default_dream_skin_tagline(),
            project_prefix: default_dream_skin_project_prefix(),
            project_label: default_dream_skin_project_label(),
            status_text: default_dream_skin_status_text(),
            quote: default_dream_skin_quote(),
            colors: if cfg!(windows) {
                None
            } else {
                Some(DreamSkinColors::default())
            },
            extra_fields,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BackendSettings {
    #[serde(rename = "codexAppPath", default)]
    pub codex_app_path: String,
    #[serde(rename = "codexExtraArgs", default)]
    pub codex_extra_args: Vec<String>,
    #[serde(rename = "providerSyncEnabled", default)]
    pub provider_sync_enabled: bool,
    #[serde(rename = "providerSyncSavedProviders", default)]
    pub provider_sync_saved_providers: Vec<String>,
    #[serde(rename = "providerSyncManualProviders", default)]
    pub provider_sync_manual_providers: Vec<String>,
    #[serde(rename = "providerSyncLastSelectedProvider", default)]
    pub provider_sync_last_selected_provider: String,
    #[serde(rename = "ccsDbPath", default)]
    pub ccs_db_path: String,
    #[serde(rename = "relayProfilesEnabled", default = "default_true")]
    pub relay_profiles_enabled: bool,
    #[serde(rename = "enhancementsEnabled", default = "default_true")]
    pub enhancements_enabled: bool,
    #[serde(rename = "codexAppPluginMarketplaceUnlock", default = "default_true")]
    pub codex_app_plugin_marketplace_unlock: bool,
    #[serde(rename = "codexAppModelWhitelistUnlock", default = "default_true")]
    pub codex_app_model_whitelist_unlock: bool,
    #[serde(rename = "codexAppSessionDelete", default = "default_true")]
    pub codex_app_session_delete: bool,
    #[serde(rename = "codexAppMarkdownExport", default = "default_true")]
    pub codex_app_markdown_export: bool,
    #[serde(rename = "codexAppPasteFix", default)]
    pub codex_app_paste_fix: bool,
    #[serde(rename = "codexAppForceChineseLocale", default = "default_true")]
    pub codex_app_force_chinese_locale: bool,
    #[serde(rename = "codexAppFastStartup", default)]
    pub codex_app_fast_startup: bool,
    #[serde(rename = "codexAppThreadIdBadge", default)]
    pub codex_app_thread_id_badge: bool,
    #[serde(rename = "codexAppConversationView", default)]
    pub codex_app_conversation_view: bool,
    #[serde(rename = "codexAppThreadScrollRestore", default = "default_true")]
    pub codex_app_thread_scroll_restore: bool,
    #[serde(rename = "codexAppZedRemoteOpen", default = "default_true")]
    pub codex_app_zed_remote_open: bool,
    #[serde(rename = "zedRemoteOpenStrategy", default)]
    pub zed_remote_open_strategy: ZedOpenStrategy,
    #[serde(rename = "zedRemoteProjectRegistryEnabled", default = "default_true")]
    pub zed_remote_project_registry_enabled: bool,
    #[serde(rename = "zedRemoteSyncToZedSettings", default)]
    pub zed_remote_sync_to_zed_settings: bool,
    #[serde(rename = "codexAppUpstreamWorktreeCreate", default = "default_true")]
    pub codex_app_upstream_worktree_create: bool,
    #[serde(rename = "codexAppNativeMenuPlacement", default = "default_true")]
    pub codex_app_native_menu_placement: bool,
    #[serde(rename = "codexAppNativeMenuLocalization", default = "default_true")]
    pub codex_app_native_menu_localization: bool,
    #[serde(rename = "codexAppServiceTierControls", default)]
    pub codex_app_service_tier_controls: bool,
    #[serde(rename = "codexAppPetRealMouseLook", default)]
    pub codex_app_pet_real_mouse_look: bool,
    #[serde(rename = "codexAppStepwiseEnabled", default)]
    pub codex_app_stepwise_enabled: bool,
    #[serde(rename = "codexAppStepwiseDirectSend", default)]
    pub codex_app_stepwise_direct_send: bool,
    #[serde(rename = "codexAppStepwiseBaseUrl", default)]
    pub codex_app_stepwise_base_url: String,
    #[serde(rename = "codexAppStepwiseApiKey", default)]
    pub codex_app_stepwise_api_key: String,
    #[serde(
        rename = "codexAppStepwiseApiKeyEnv",
        default = "default_stepwise_api_key_env",
        deserialize_with = "empty_as_default_stepwise_api_key_env"
    )]
    pub codex_app_stepwise_api_key_env: String,
    #[serde(
        rename = "codexAppStepwiseProtocol",
        default = "default_stepwise_protocol",
        deserialize_with = "deserialize_stepwise_protocol"
    )]
    pub codex_app_stepwise_protocol: String,
    #[serde(rename = "codexAppStepwiseModel", default)]
    pub codex_app_stepwise_model: String,
    #[serde(
        rename = "codexAppStepwiseMaxItems",
        default = "default_stepwise_max_items",
        deserialize_with = "deserialize_stepwise_max_items"
    )]
    pub codex_app_stepwise_max_items: u8,
    #[serde(
        rename = "codexAppStepwiseMaxInputChars",
        default = "default_stepwise_max_input_chars",
        deserialize_with = "deserialize_stepwise_max_input_chars"
    )]
    pub codex_app_stepwise_max_input_chars: u32,
    #[serde(
        rename = "codexAppStepwiseMaxOutputTokens",
        default = "default_stepwise_max_output_tokens",
        deserialize_with = "deserialize_stepwise_max_output_tokens"
    )]
    pub codex_app_stepwise_max_output_tokens: u32,
    #[serde(
        rename = "codexAppStepwiseTimeoutMs",
        default = "default_stepwise_timeout_ms",
        deserialize_with = "deserialize_stepwise_timeout_ms"
    )]
    pub codex_app_stepwise_timeout_ms: u64,
    #[serde(rename = "codexAppImageOverlayEnabled", default)]
    pub codex_app_image_overlay_enabled: bool,
    #[serde(rename = "codexAppImageOverlayPath", default)]
    pub codex_app_image_overlay_path: String,
    #[serde(
        rename = "codexAppImageOverlayOpacity",
        default = "default_image_overlay_opacity",
        deserialize_with = "deserialize_image_overlay_opacity"
    )]
    pub codex_app_image_overlay_opacity: u8,
    #[serde(
        rename = "codexAppImageOverlayFitMode",
        default = "default_image_overlay_fit_mode",
        deserialize_with = "deserialize_image_overlay_fit_mode"
    )]
    pub codex_app_image_overlay_fit_mode: String,
    #[serde(rename = "codexAppDreamSkinEnabled", default)]
    pub codex_app_dream_skin_enabled: bool,
    #[serde(rename = "codexAppDreamSkinPaused", default)]
    pub codex_app_dream_skin_paused: bool,
    #[serde(
        rename = "codexAppDreamSkinTheme",
        default = "default_dream_skin_theme",
        deserialize_with = "deserialize_dream_skin_theme"
    )]
    pub codex_app_dream_skin_theme: String,
    #[serde(rename = "codexAppDreamSkinThemeConfig", default)]
    pub codex_app_dream_skin_theme_config: DreamSkinThemeConfig,
    #[serde(rename = "codexAppDreamSkinImagePath", default)]
    pub codex_app_dream_skin_image_path: String,
    #[serde(rename = "codexGoalsEnabled", default)]
    pub codex_goals_enabled: bool,
    #[serde(rename = "weixinConnectEnabled", default)]
    pub weixin_connect_enabled: bool,
    #[serde(
        rename = "weixinConnectBaseUrl",
        default = "default_weixin_connect_base_url"
    )]
    pub weixin_connect_base_url: String,
    #[serde(rename = "weixinConnectToken", default)]
    pub weixin_connect_token: String,
    #[serde(rename = "weixinConnectAccountId", default)]
    pub weixin_connect_account_id: String,
    #[serde(rename = "weixinConnectAllowFrom", default)]
    pub weixin_connect_allow_from: String,
    #[serde(rename = "weixinConnectRouteTag", default)]
    pub weixin_connect_route_tag: String,
    #[serde(rename = "weixinConnectWorkDir", default)]
    pub weixin_connect_work_dir: String,
    #[serde(rename = "weixinConnectModel", default)]
    pub weixin_connect_model: String,
    #[serde(
        rename = "weixinConnectSandbox",
        default = "default_weixin_connect_sandbox"
    )]
    pub weixin_connect_sandbox: String,
    #[serde(rename = "weixinConnectCodexPath", default)]
    pub weixin_connect_codex_path: String,
    #[serde(rename = "launchMode", default)]
    pub launch_mode: LaunchMode,
    #[serde(rename = "relayBaseUrl", default = "default_relay_base_url")]
    pub relay_base_url: String,
    #[serde(rename = "relayApiKey", default)]
    pub relay_api_key: String,
    #[serde(rename = "relayProfiles", default = "default_relay_profiles")]
    pub relay_profiles: Vec<RelayProfile>,
    #[serde(rename = "relayCommonConfigContents", default)]
    pub relay_common_config_contents: String,
    #[serde(rename = "relayContextConfigContents", default)]
    pub relay_context_config_contents: String,
    #[serde(rename = "activeRelayId", default = "default_active_relay_id")]
    pub active_relay_id: String,
    #[serde(rename = "aggregateRelayProfiles", default)]
    pub aggregate_relay_profiles: Vec<AggregateRelayProfile>,
    #[serde(rename = "activeAggregateRelayId", default)]
    pub active_aggregate_relay_id: String,
    #[serde(rename = "relayTestModel", default = "default_relay_test_model")]
    pub relay_test_model: String,
}

impl Default for BackendSettings {
    fn default() -> Self {
        Self {
            codex_app_path: String::new(),
            codex_extra_args: Vec::new(),
            provider_sync_enabled: false,
            provider_sync_saved_providers: Vec::new(),
            provider_sync_manual_providers: Vec::new(),
            provider_sync_last_selected_provider: String::new(),
            ccs_db_path: String::new(),
            relay_profiles_enabled: true,
            enhancements_enabled: true,
            codex_app_plugin_marketplace_unlock: true,
            codex_app_model_whitelist_unlock: true,
            codex_app_session_delete: true,
            codex_app_markdown_export: true,
            codex_app_paste_fix: false,
            codex_app_force_chinese_locale: true,
            codex_app_fast_startup: false,
            codex_app_thread_id_badge: false,
            codex_app_conversation_view: false,
            codex_app_thread_scroll_restore: true,
            codex_app_zed_remote_open: true,
            zed_remote_open_strategy: ZedOpenStrategy::AddToFocusedWorkspace,
            zed_remote_project_registry_enabled: true,
            zed_remote_sync_to_zed_settings: false,
            codex_app_upstream_worktree_create: true,
            codex_app_native_menu_placement: true,
            codex_app_native_menu_localization: true,
            codex_app_service_tier_controls: false,
            codex_app_pet_real_mouse_look: false,
            codex_app_stepwise_enabled: false,
            codex_app_stepwise_direct_send: false,
            codex_app_stepwise_base_url: String::new(),
            codex_app_stepwise_api_key: String::new(),
            codex_app_stepwise_api_key_env: default_stepwise_api_key_env(),
            codex_app_stepwise_protocol: default_stepwise_protocol(),
            codex_app_stepwise_model: String::new(),
            codex_app_stepwise_max_items: default_stepwise_max_items(),
            codex_app_stepwise_max_input_chars: default_stepwise_max_input_chars(),
            codex_app_stepwise_max_output_tokens: default_stepwise_max_output_tokens(),
            codex_app_stepwise_timeout_ms: default_stepwise_timeout_ms(),
            codex_app_image_overlay_enabled: false,
            codex_app_image_overlay_path: String::new(),
            codex_app_image_overlay_opacity: default_image_overlay_opacity(),
            codex_app_image_overlay_fit_mode: default_image_overlay_fit_mode(),
            codex_app_dream_skin_enabled: false,
            codex_app_dream_skin_paused: false,
            codex_app_dream_skin_theme: default_dream_skin_theme(),
            codex_app_dream_skin_theme_config: DreamSkinThemeConfig::default(),
            codex_app_dream_skin_image_path: String::new(),
            codex_goals_enabled: false,
            weixin_connect_enabled: false,
            weixin_connect_base_url: default_weixin_connect_base_url(),
            weixin_connect_token: String::new(),
            weixin_connect_account_id: String::new(),
            weixin_connect_allow_from: String::new(),
            weixin_connect_route_tag: String::new(),
            weixin_connect_work_dir: String::new(),
            weixin_connect_model: String::new(),
            weixin_connect_sandbox: default_weixin_connect_sandbox(),
            weixin_connect_codex_path: String::new(),
            launch_mode: LaunchMode::Patch,
            relay_base_url: default_relay_base_url(),
            relay_api_key: String::new(),
            relay_profiles: default_relay_profiles(),
            relay_common_config_contents: String::new(),
            relay_context_config_contents: String::new(),
            active_relay_id: default_active_relay_id(),
            aggregate_relay_profiles: Vec::new(),
            active_aggregate_relay_id: String::new(),
            relay_test_model: default_relay_test_model(),
        }
    }
}

impl BackendSettings {
    pub fn active_relay_profile(&self) -> RelayProfile {
        if self.active_relay_id == default_active_relay_id()
            && self.relay_profiles.len() == 1
            && self.relay_profiles[0] == RelayProfile::default()
            && (!self.relay_api_key.is_empty() || self.relay_base_url != default_relay_base_url())
        {
            return RelayProfile {
                id: default_active_relay_id(),
                name: "默认中转".to_string(),
                model: String::new(),
                base_url: if self.relay_base_url.is_empty() {
                    default_relay_base_url()
                } else {
                    self.relay_base_url.clone()
                },
                upstream_base_url: if self.relay_base_url.is_empty() {
                    default_relay_base_url()
                } else {
                    self.relay_base_url.clone()
                },
                api_key: self.relay_api_key.clone(),
                protocol: RelayProtocol::Responses,
                responses_reasoning_policy: ResponsesReasoningPolicy::default(),
                responses_wire_policy: ResponsesWirePolicy::default(),
                custom_tools_as_functions: false,
                native_agent_interop: NativeAgentInterop::default(),
                relay_mode: RelayMode::MixedApi,
                official_mix_api_key: true,
                no_auth: false,
                hide_official_usage_alert: false,
                test_model: String::new(),
                config_contents: String::new(),
                auth_contents: String::new(),
                use_common_config: true,
                context_window: String::new(),
                auto_compact_limit: String::new(),
                new_context_management: false,
                model_insert_mode: RelayModelInsertMode::Patch,
                model_list: String::new(),
                model_windows: String::new(),
                model_vlm: String::new(),
                vlm_api_key: String::new(),
                vlm_model: String::new(),
                vlm_base_url: String::new(),
                user_agent: String::new(),
                sub2api_enabled: false,
                sub2api_multiplier: String::new(),
                model_routes: Vec::new(),
                model_aliases: Vec::new(),
            };
        }

        if let Some(profile) = self
            .relay_profiles
            .iter()
            .find(|profile| profile.id == self.active_relay_id)
        {
            let mut profile = profile.clone();
            self.apply_aggregate_context_fallback(&mut profile);
            if profile.relay_mode == RelayMode::Aggregate
                && self
                    .active_aggregate_relay_profile()
                    .is_some_and(|aggregate| {
                        aggregate.session_provider == RelaySessionProvider::Openai
                    })
                && !crate::relay_config::auth_contents_looks_like_chatgpt_auth(
                    &profile.auth_contents,
                )
            {
                if let Some(official) = self.relay_profiles.iter().find(|candidate| {
                    candidate.id == default_active_relay_id()
                        && candidate.relay_mode == RelayMode::Official
                        && crate::relay_config::auth_contents_looks_like_chatgpt_auth(
                            &candidate.auth_contents,
                        )
                }) {
                    profile.auth_contents = official.auth_contents.clone();
                }
            }
            return profile;
        }

        RelayProfile {
            id: if self.active_relay_id.is_empty() {
                default_active_relay_id()
            } else {
                self.active_relay_id.clone()
            },
            name: "默认中转".to_string(),
            model: String::new(),
            base_url: if self.relay_base_url.is_empty() {
                default_relay_base_url()
            } else {
                self.relay_base_url.clone()
            },
            upstream_base_url: if self.relay_base_url.is_empty() {
                default_relay_base_url()
            } else {
                self.relay_base_url.clone()
            },
            api_key: self.relay_api_key.clone(),
            protocol: RelayProtocol::Responses,
            responses_reasoning_policy: ResponsesReasoningPolicy::default(),
            responses_wire_policy: ResponsesWirePolicy::default(),
            custom_tools_as_functions: false,
            native_agent_interop: NativeAgentInterop::default(),
            relay_mode: RelayMode::Official,
            official_mix_api_key: false,
            no_auth: false,
            hide_official_usage_alert: false,
            test_model: String::new(),
            config_contents: String::new(),
            auth_contents: String::new(),
            use_common_config: true,
            context_window: String::new(),
            auto_compact_limit: String::new(),
            new_context_management: false,
            model_insert_mode: RelayModelInsertMode::Patch,
            model_list: String::new(),
            model_windows: String::new(),
            model_vlm: String::new(),
            vlm_api_key: String::new(),
            vlm_model: String::new(),
            vlm_base_url: String::new(),
            user_agent: String::new(),
            sub2api_enabled: false,
            sub2api_multiplier: String::new(),
            model_routes: Vec::new(),
            model_aliases: Vec::new(),
        }
    }

    pub fn active_aggregate_relay_profile(&self) -> Option<AggregateRelayProfile> {
        let active_relay = self
            .relay_profiles
            .iter()
            .find(|profile| profile.id == self.active_relay_id)?;
        if active_relay.relay_mode != RelayMode::Aggregate {
            return None;
        }

        let active_aggregate_id = if self.active_aggregate_relay_id.trim().is_empty() {
            active_relay.id.as_str()
        } else {
            self.active_aggregate_relay_id.trim()
        };

        if active_aggregate_id != active_relay.id {
            return None;
        }

        self.aggregate_relay_profiles
            .iter()
            .find(|profile| profile.id == active_aggregate_id)
            .cloned()
    }

    fn apply_aggregate_context_fallback(&self, profile: &mut RelayProfile) {
        if profile.relay_mode != RelayMode::Aggregate {
            return;
        }
        let Some(aggregate) = self.active_aggregate_relay_profile() else {
            return;
        };
        let preferred_model = relay_profile_models(profile).into_iter().next();
        let member_profiles = aggregate
            .members
            .iter()
            .filter_map(|member| {
                self.relay_profiles
                    .iter()
                    .find(|candidate| candidate.id == member.relay_id)
            })
            .collect::<Vec<_>>();
        let mut aggregate_models = Vec::new();
        for member in &member_profiles {
            for model in relay_profile_models(member) {
                if !aggregate_models.contains(&model) {
                    aggregate_models.push(model);
                }
            }
        }
        if !aggregate_models.is_empty() {
            profile.model = preferred_model
                .filter(|model| aggregate_models.contains(model))
                .unwrap_or_else(|| aggregate_models[0].clone());
            profile.model_list = aggregate_models.join("\n");
        }
        let mut source: Option<(&RelayProfile, u64)> = None;
        for member_profile in member_profiles {
            let Some(context_window) = member_profile
                .context_window
                .trim()
                .parse::<u64>()
                .ok()
                .filter(|value| *value > 0)
            else {
                continue;
            };
            if source.is_none_or(|(_, selected_window)| context_window > selected_window) {
                source = Some((member_profile, context_window));
            }
        }
        let Some((source, source_window)) = source else {
            return;
        };

        let inherited_context = profile.context_window.trim().is_empty();
        let explicit_context_matches_source = profile
            .context_window
            .trim()
            .parse::<u64>()
            .ok()
            .is_some_and(|value| value == source_window);
        if inherited_context {
            profile.context_window = source.context_window.clone();
        }
        if profile.auto_compact_limit.trim().is_empty()
            && (inherited_context || explicit_context_matches_source)
        {
            profile.auto_compact_limit = source.auto_compact_limit.clone();
        }
        if inherited_context {
            if profile.model.trim().is_empty() {
                profile.model = source.model.clone();
            }
            if profile.model_windows.trim().is_empty() {
                profile.model_windows = source.model_windows.clone();
            }
        }
    }

    pub fn active_relay_session_provider(&self) -> RelaySessionProvider {
        if let Some(profile) = self.active_aggregate_relay_profile() {
            return profile.session_provider;
        }
        if self
            .active_relay_profile()
            .config_contents
            .parse::<DocumentMut>()
            .ok()
            .is_some_and(|doc| {
                doc.get("model_provider")
                    .and_then(Item::as_str)
                    .map(str::trim)
                    .is_some_and(|provider| provider == "openai")
            })
        {
            RelaySessionProvider::Openai
        } else {
            RelaySessionProvider::Custom
        }
    }

    pub fn active_relay_uses_protocol_proxy(&self) -> bool {
        self.active_aggregate_relay_profile().is_some()
            || self.active_relay_profile().protocol == RelayProtocol::ChatCompletions
            || self.active_relay_profile().has_model_routes()
            || self.active_relay_profile().uses_no_auth()
            || self.active_relay_session_provider() == RelaySessionProvider::Openai
    }
}

fn relay_profile_models(profile: &RelayProfile) -> Vec<String> {
    let mut models = Vec::new();
    for raw in std::iter::once(profile.model.as_str())
        .chain(profile.model_list.split(['\r', '\n', ',']).map(str::trim))
    {
        let (model, _) = crate::model_suffix::parse_model_suffix(raw);
        if !model.is_empty() && !models.contains(&model) {
            models.push(model);
        }
    }
    models
}

pub fn default_stepwise_api_key_env() -> String {
    "CODEX_STEPWISE_API_KEY".to_string()
}

pub fn default_stepwise_protocol() -> String {
    "chat_completions".to_string()
}

pub fn normalize_stepwise_protocol(value: &str) -> String {
    match value.trim() {
        "chat_completions" | "responses" | "anthropic_messages" | "auto" => {
            value.trim().to_string()
        }
        _ => default_stepwise_protocol(),
    }
}

pub fn default_stepwise_max_items() -> u8 {
    6
}

pub fn default_stepwise_max_input_chars() -> u32 {
    6000
}

pub fn default_stepwise_max_output_tokens() -> u32 {
    500
}

pub fn default_stepwise_timeout_ms() -> u64 {
    8000
}

fn default_image_overlay_opacity() -> u8 {
    35
}

fn clamp_image_overlay_opacity(value: u8) -> u8 {
    value.clamp(1, 100)
}

pub fn default_image_overlay_fit_mode() -> String {
    "fit".to_string()
}

fn normalize_image_overlay_fit_mode(value: &str) -> String {
    match value {
        "fill" | "fit" | "stretch" | "tile" | "center" => value.to_string(),
        _ => default_image_overlay_fit_mode(),
    }
}

pub fn default_dream_skin_theme() -> String {
    "pink".to_string()
}

fn default_dream_skin_schema_version() -> u8 {
    1
}

#[cfg(windows)]
fn default_dream_skin_id() -> String {
    "preset-arina-hashimoto".to_string()
}

#[cfg(not(windows))]
fn default_dream_skin_id() -> String {
    "custom-1784123441349".to_string()
}

#[cfg(windows)]
fn default_dream_skin_name() -> String {
    "桥本有菜".to_string()
}

#[cfg(not(windows))]
fn default_dream_skin_name() -> String {
    "Dream Skin".to_string()
}

pub fn resolve_dream_skin_style_preset(id: &str, style_preset: &str) -> String {
    let style_preset = style_preset.trim();
    if !style_preset.is_empty() && style_preset != "dream-original" {
        return style_preset.to_string();
    }

    match id.trim() {
        "caishen-lite" => "caishen-lite",
        "caishen-max" => "caishen-max",
        "caishen-readable" => "caishen-readable",
        "export-night" => "export-night",
        "global-founder-bright" => "global-founder-bright",
        "mythic-guardian-noir" => "mythic-guardian-noir",
        "codex-snow-skin" => "codex-snow",
        "glass-vision" => "glass-vision",
        "preset-midnight-aurora" => "midnight-aurora",
        "preset-amber-dusk" => "amber-dusk",
        "preset-forest-mist" => "forest-mist",
        "preset-cyber-neon" => "cyber-neon",
        "preset-sakura-dawn" => "sakura-dawn",
        _ => "dream-original",
    }
    .to_string()
}

fn default_dream_skin_brand_subtitle() -> String {
    "CODEX DREAM SKIN".to_string()
}

#[cfg(windows)]
fn default_dream_skin_tagline() -> String {
    "把柔光与玫瑰带进今天的工作台。".to_string()
}

#[cfg(not(windows))]
fn default_dream_skin_tagline() -> String {
    "把喜欢的画面变成可交互的 Codex 工作台。".to_string()
}

fn default_dream_skin_project_prefix() -> String {
    "选择项目 · ".to_string()
}

fn default_dream_skin_project_label() -> String {
    "◉  选择项目".to_string()
}

#[cfg(windows)]
fn default_dream_skin_status_text() -> String {
    "DREAM SKIN ONLINE".to_string()
}

#[cfg(not(windows))]
fn default_dream_skin_status_text() -> String {
    "THEME ONLINE".to_string()
}

#[cfg(windows)]
fn default_dream_skin_quote() -> String {
    "MAKE SOMETHING WONDERFUL".to_string()
}

#[cfg(not(windows))]
fn default_dream_skin_quote() -> String {
    "Make something wonderful".to_string()
}

fn normalize_dream_skin_theme(value: &str) -> String {
    match value.trim() {
        "pink" | "luckyGod" | "redWhite" | "clearGlass" | "inspiration" | "purpleNight"
        | "miku" | "blackGold" => value.trim().to_string(),
        _ => default_dream_skin_theme(),
    }
}

pub fn clamp_stepwise_max_items(value: u8) -> u8 {
    value.min(default_stepwise_max_items())
}

pub fn clamp_stepwise_max_input_chars(value: u32) -> u32 {
    value.clamp(1000, 24000)
}

pub fn clamp_stepwise_max_output_tokens(value: u32) -> u32 {
    value.clamp(100, 4000)
}

pub fn clamp_stepwise_timeout_ms(value: u64) -> u64 {
    value.clamp(1000, 60000)
}

pub fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

pub fn default_relay_base_url() -> String {
    String::new()
}

fn default_weixin_connect_base_url() -> String {
    crate::connect::DEFAULT_WEIXIN_BASE_URL.to_string()
}

fn default_weixin_connect_sandbox() -> String {
    "read-only".to_string()
}

pub fn default_active_relay_id() -> String {
    "default".to_string()
}

pub fn default_relay_test_model() -> String {
    "gpt-5.4-mini".to_string()
}

pub fn default_relay_profiles() -> Vec<RelayProfile> {
    vec![RelayProfile::default()]
}

pub fn default_aggregate_member_weight() -> u32 {
    1
}

pub fn empty_as_default_stepwise_api_key_env<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    Ok(value
        .filter(|value| !value.is_empty())
        .unwrap_or_else(default_stepwise_api_key_env))
}

fn deserialize_stepwise_protocol<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?
        .map(|value| normalize_stepwise_protocol(&value))
        .unwrap_or_else(default_stepwise_protocol))
}

fn deserialize_image_overlay_opacity<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<u8>::deserialize(deserializer)?
        .map(clamp_image_overlay_opacity)
        .unwrap_or_else(default_image_overlay_opacity))
}

fn deserialize_image_overlay_fit_mode<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?
        .map(|value| normalize_image_overlay_fit_mode(&value))
        .unwrap_or_else(default_image_overlay_fit_mode))
}

fn deserialize_dream_skin_theme<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?
        .map(|value| normalize_dream_skin_theme(&value))
        .unwrap_or_else(default_dream_skin_theme))
}

fn deserialize_stepwise_max_items<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<u8>::deserialize(deserializer)?
        .map(clamp_stepwise_max_items)
        .unwrap_or_else(default_stepwise_max_items))
}

fn deserialize_stepwise_max_input_chars<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<u32>::deserialize(deserializer)?
        .map(clamp_stepwise_max_input_chars)
        .unwrap_or_else(default_stepwise_max_input_chars))
}

fn deserialize_stepwise_max_output_tokens<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<u32>::deserialize(deserializer)?
        .map(clamp_stepwise_max_output_tokens)
        .unwrap_or_else(default_stepwise_max_output_tokens))
}

fn deserialize_stepwise_timeout_ms<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<u64>::deserialize(deserializer)?
        .map(clamp_stepwise_timeout_ms)
        .unwrap_or_else(default_stepwise_timeout_ms))
}

fn deserialize_profile_api_key<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

pub fn normalize_codex_extra_args(args: &[String]) -> Vec<String> {
    args.iter()
        .map(|arg| arg.trim())
        .filter(|arg| !arg.is_empty())
        .map(ToString::to_string)
        .collect()
}

#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl Default for SettingsStore {
    fn default() -> Self {
        Self::new(crate::paths::default_settings_path())
    }
}

impl SettingsStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// GUI and CLI control operations share a cross-process write lock.
    pub fn control_lock(&self) -> anyhow::Result<fs::File> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.path.with_extension("control.lock"))?;
        fs2::FileExt::try_lock_exclusive(&file)
            .context("Another Codex++ control operation is writing settings; retry later")?;
        Ok(file)
    }

    pub fn load(&self) -> anyhow::Result<BackendSettings> {
        let contents = match fs::read_to_string(&self.path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BackendSettings::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read settings {}", self.path.display()));
            }
        };

        Ok(normalize_settings_config_sections(
            serde_json::from_str(&contents).unwrap_or_default(),
        ))
    }

    pub fn save(&self, settings: &BackendSettings) -> anyhow::Result<()> {
        let mut settings = normalize_settings_config_sections(settings.clone());
        settings.codex_extra_args = normalize_codex_extra_args(&settings.codex_extra_args);
        if let Ok(current) = self.load() {
            preserve_model_route_states(&mut settings.relay_profiles, &current.relay_profiles);
        }
        let bytes = serde_json::to_vec_pretty(&settings)?;
        atomic_write(&self.path, &bytes)
    }

    pub fn model_routes_list(&self, provider_id: &str) -> anyhow::Result<RelayModelRoutesResult> {
        let settings = self.load()?;
        let provider = settings
            .relay_profiles
            .iter()
            .find(|profile| profile.id == provider_id)
            .context("Provider not found")?;
        let observed_at = unix_time_ms();
        Ok(RelayModelRoutesResult {
            status: "ok",
            provider_id: provider.id.clone(),
            provider_name: provider.name.clone(),
            observed_at,
            routes: provider
                .model_routes
                .iter()
                .map(|route| model_route_status(&settings, route, observed_at))
                .collect(),
        })
    }

    pub fn model_route_set(
        &self,
        request: &SetRelayModelRouteRequest,
        dry_run: bool,
    ) -> anyhow::Result<SetRelayModelRouteResult> {
        let recovery_choices = usize::from(request.restore_at.is_some())
            + usize::from(request.duration_seconds.is_some())
            + usize::from(request.permanent);
        if request.enabled && recovery_choices != 0 {
            anyhow::bail!("Recovery options are only valid when disabling a route");
        }
        if recovery_choices > 1 {
            anyhow::bail!("restoreAt, durationSeconds and permanent are mutually exclusive");
        }
        if request.id.trim().is_empty() || request.model.trim().is_empty() {
            anyhow::bail!("Provider id and model are required");
        }
        let observed_at = unix_time_ms();
        let restore_at = if request.enabled || request.permanent {
            None
        } else if let Some(restore_at) = request.restore_at {
            if restore_at <= observed_at {
                anyhow::bail!("restoreAt must be a future UTC Unix millisecond timestamp");
            }
            Some(restore_at)
        } else if let Some(duration_seconds) = request.duration_seconds {
            if duration_seconds == 0 {
                anyhow::bail!("durationSeconds must be positive");
            }
            Some(observed_at.saturating_add(duration_seconds.saturating_mul(1000)))
        } else {
            Some(observed_at.saturating_add(5 * 60 * 60 * 1000))
        };

        let _lock = self.control_lock()?;
        let mut raw = self.load_raw_object()?;
        let profiles = raw
            .get_mut("relayProfiles")
            .and_then(Value::as_array_mut)
            .context("Provider list is missing")?;
        let provider = profiles
            .iter_mut()
            .find(|value| {
                value.get("id").and_then(Value::as_str).map(str::trim) == Some(request.id.trim())
            })
            .context("Provider not found")?;
        let routes = provider
            .get_mut("modelRoutes")
            .and_then(Value::as_array_mut)
            .context("Model route not found")?;
        let route = routes
            .iter_mut()
            .find(|value| {
                value.get("model").and_then(Value::as_str).map(str::trim)
                    == Some(request.model.trim())
            })
            .context("Model route not found")?;
        let route_object = route.as_object_mut().context("Invalid model route")?;
        route_object.insert("enabled".to_string(), Value::Bool(request.enabled));
        match restore_at {
            Some(value) => {
                route_object.insert("restoreAt".to_string(), Value::Number(value.into()));
            }
            None => {
                route_object.remove("restoreAt");
            }
        }
        let candidate = normalize_settings_config_sections(serde_json::from_value::<
            BackendSettings,
        >(Value::Object(raw.clone()))?);
        let provider = candidate
            .relay_profiles
            .iter()
            .find(|profile| profile.id.trim() == request.id.trim())
            .context("Provider not found")?;
        let route = provider
            .model_routes
            .iter()
            .find(|route| route.model.trim() == request.model.trim())
            .context("Model route not found")?;
        let status = model_route_status(&candidate, route, observed_at);
        if !dry_run {
            atomic_write(&self.path, &serde_json::to_vec_pretty(&Value::Object(raw))?)?;
        }
        Ok(SetRelayModelRouteResult {
            status: "ok",
            provider_id: request.id.trim().to_string(),
            observed_at,
            route: status,
            dry_run,
            restart_requested: false,
            applied_live_config: false,
        })
    }

    pub fn update(&self, payload: Value) -> anyhow::Result<BackendSettings> {
        let Value::Object(payload) = payload else {
            return self.load();
        };

        let mut raw = self.load_raw_object()?;
        merge_known_setting_fields(&mut raw, &payload);
        let settings = normalize_settings_config_sections(
            serde_json::from_value(Value::Object(raw.clone())).unwrap_or_default(),
        );
        raw.insert(
            "relayCommonConfigContents".to_string(),
            Value::String(settings.relay_common_config_contents.clone()),
        );
        raw.insert(
            "relayContextConfigContents".to_string(),
            Value::String(settings.relay_context_config_contents.clone()),
        );
        let bytes = serde_json::to_vec_pretty(&Value::Object(raw))?;
        atomic_write(&self.path, &bytes)?;
        Ok(settings)
    }

    fn load_raw_object(&self) -> anyhow::Result<Map<String, Value>> {
        let contents = match fs::read_to_string(&self.path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(settings_to_object(&BackendSettings::default()));
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read settings {}", self.path.display()));
            }
        };

        match serde_json::from_str::<Value>(&contents) {
            Ok(Value::Object(map)) => Ok(map),
            Ok(_) | Err(_) => Ok(settings_to_object(&BackendSettings::default())),
        }
    }
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn model_route_status(
    settings: &BackendSettings,
    route: &RelayModelRoute,
    observed_at: u64,
) -> RelayModelRouteStatus {
    let enabled = route.is_effectively_enabled_at(observed_at);
    let restore_at = (!enabled).then_some(route.restore_at).flatten();
    RelayModelRouteStatus {
        model: route.model.clone(),
        target_relay_id: route.target_relay_id.clone(),
        target_relay_name: settings
            .relay_profiles
            .iter()
            .find(|profile| profile.id == route.target_relay_id)
            .map(|profile| profile.name.clone())
            .unwrap_or_default(),
        target_model: route.target_model.clone(),
        enabled,
        restore_at,
        permanent: !enabled && route.restore_at.is_none(),
        remaining_seconds: restore_at
            .map(|value| value.saturating_sub(observed_at).saturating_add(999) / 1000)
            .unwrap_or(0),
    }
}

fn preserve_model_route_states(next: &mut [RelayProfile], current: &[RelayProfile]) {
    for profile in next {
        let Some(current_profile) = current.iter().find(|item| item.id == profile.id) else {
            continue;
        };
        for route in &mut profile.model_routes {
            if let Some(current_route) = current_profile
                .model_routes
                .iter()
                .find(|item| item.model == route.model)
            {
                route.enabled = current_route.enabled;
                route.restore_at = current_route.restore_at;
            }
        }
    }
}

fn merge_known_setting_fields(target: &mut Map<String, Value>, source: &Map<String, Value>) {
    target.remove("codexAppPluginAutoExpand");
    target.remove("computerUseGuardEnabled");
    if let Some(value) = source.get("codexAppPath").and_then(Value::as_str) {
        target.insert("codexAppPath".to_string(), Value::String(value.to_string()));
    }
    if let Some(value) = source.get("codexExtraArgs").and_then(Value::as_array) {
        let args = value
            .iter()
            .filter_map(Value::as_str)
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        target.insert(
            "codexExtraArgs".to_string(),
            Value::Array(
                normalize_codex_extra_args(&args)
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    if let Some(value) = source.get("providerSyncEnabled").and_then(Value::as_bool) {
        target.insert("providerSyncEnabled".to_string(), Value::Bool(value));
    }
    if let Some(value) = source.get("ccsDbPath").and_then(Value::as_str) {
        target.insert(
            "ccsDbPath".to_string(),
            Value::String(value.trim().to_string()),
        );
    }
    if let Some(value) = source.get("relayProfilesEnabled").and_then(Value::as_bool) {
        target.insert("relayProfilesEnabled".to_string(), Value::Bool(value));
    }
    if let Some(value) = source.get("enhancementsEnabled").and_then(Value::as_bool) {
        target.insert("enhancementsEnabled".to_string(), Value::Bool(value));
    }
    merge_bool_setting(target, source, "codexAppPluginMarketplaceUnlock");
    merge_bool_setting(target, source, "codexAppModelWhitelistUnlock");
    merge_bool_setting(target, source, "codexAppSessionDelete");
    merge_bool_setting(target, source, "codexAppMarkdownExport");
    merge_bool_setting(target, source, "codexAppPasteFix");
    merge_bool_setting(target, source, "codexAppForceChineseLocale");
    merge_bool_setting(target, source, "codexAppFastStartup");
    merge_bool_setting(target, source, "codexAppThreadIdBadge");
    merge_bool_setting(target, source, "codexAppConversationView");
    merge_bool_setting(target, source, "codexAppThreadScrollRestore");
    merge_bool_setting(target, source, "codexAppZedRemoteOpen");
    if let Some(value) = source.get("zedRemoteOpenStrategy") {
        if serde_json::from_value::<ZedOpenStrategy>(value.clone()).is_ok() {
            target.insert("zedRemoteOpenStrategy".to_string(), value.clone());
        }
    }
    merge_bool_setting(target, source, "zedRemoteProjectRegistryEnabled");
    merge_bool_setting(target, source, "zedRemoteSyncToZedSettings");
    merge_bool_setting(target, source, "codexAppUpstreamWorktreeCreate");
    merge_bool_setting(target, source, "codexAppNativeMenuPlacement");
    merge_bool_setting(target, source, "codexAppNativeMenuLocalization");
    merge_bool_setting(target, source, "codexAppServiceTierControls");
    merge_bool_setting(target, source, "codexAppPetRealMouseLook");
    merge_bool_setting(target, source, "codexAppStepwiseEnabled");
    merge_bool_setting(target, source, "codexAppStepwiseDirectSend");
    if let Some(value) = source
        .get("codexAppStepwiseBaseUrl")
        .and_then(Value::as_str)
    {
        target.insert(
            "codexAppStepwiseBaseUrl".to_string(),
            Value::String(value.trim().trim_end_matches('/').to_string()),
        );
    }
    if let Some(value) = source.get("codexAppStepwiseApiKey").and_then(Value::as_str) {
        target.insert(
            "codexAppStepwiseApiKey".to_string(),
            Value::String(value.trim().to_string()),
        );
    }
    if let Some(value) = source
        .get("codexAppStepwiseApiKeyEnv")
        .and_then(Value::as_str)
    {
        target.insert(
            "codexAppStepwiseApiKeyEnv".to_string(),
            Value::String(if value.trim().is_empty() {
                default_stepwise_api_key_env()
            } else {
                value.trim().to_string()
            }),
        );
    }
    if let Some(value) = source
        .get("codexAppStepwiseProtocol")
        .and_then(Value::as_str)
    {
        target.insert(
            "codexAppStepwiseProtocol".to_string(),
            Value::String(normalize_stepwise_protocol(value)),
        );
    }
    if let Some(value) = source.get("codexAppStepwiseModel").and_then(Value::as_str) {
        target.insert(
            "codexAppStepwiseModel".to_string(),
            Value::String(value.trim().to_string()),
        );
    }
    if let Some(value) = source
        .get("codexAppStepwiseMaxItems")
        .and_then(Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
    {
        target.insert(
            "codexAppStepwiseMaxItems".to_string(),
            Value::Number(serde_json::Number::from(clamp_stepwise_max_items(value))),
        );
    }
    if let Some(value) = source
        .get("codexAppStepwiseMaxInputChars")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
    {
        target.insert(
            "codexAppStepwiseMaxInputChars".to_string(),
            Value::Number(serde_json::Number::from(clamp_stepwise_max_input_chars(
                value,
            ))),
        );
    }
    if let Some(value) = source
        .get("codexAppStepwiseMaxOutputTokens")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
    {
        target.insert(
            "codexAppStepwiseMaxOutputTokens".to_string(),
            Value::Number(serde_json::Number::from(clamp_stepwise_max_output_tokens(
                value,
            ))),
        );
    }
    if let Some(value) = source
        .get("codexAppStepwiseTimeoutMs")
        .and_then(Value::as_u64)
    {
        target.insert(
            "codexAppStepwiseTimeoutMs".to_string(),
            Value::Number(serde_json::Number::from(clamp_stepwise_timeout_ms(value))),
        );
    }
    merge_bool_setting(target, source, "codexAppImageOverlayEnabled");
    if let Some(value) = source
        .get("codexAppImageOverlayPath")
        .and_then(Value::as_str)
    {
        target.insert(
            "codexAppImageOverlayPath".to_string(),
            Value::String(value.to_string()),
        );
    }
    if let Some(value) = source
        .get("codexAppImageOverlayOpacity")
        .and_then(Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
    {
        target.insert(
            "codexAppImageOverlayOpacity".to_string(),
            Value::Number(serde_json::Number::from(clamp_image_overlay_opacity(value))),
        );
    }
    if let Some(value) = source
        .get("codexAppImageOverlayFitMode")
        .and_then(Value::as_str)
    {
        target.insert(
            "codexAppImageOverlayFitMode".to_string(),
            Value::String(normalize_image_overlay_fit_mode(value)),
        );
    }
    merge_bool_setting(target, source, "codexAppDreamSkinEnabled");
    merge_bool_setting(target, source, "codexAppDreamSkinPaused");
    if let Some(value) = source.get("codexAppDreamSkinTheme").and_then(Value::as_str) {
        target.insert(
            "codexAppDreamSkinTheme".to_string(),
            Value::String(normalize_dream_skin_theme(value)),
        );
    }
    if let Some(value) = source.get("codexAppDreamSkinThemeConfig")
        && serde_json::from_value::<DreamSkinThemeConfig>(value.clone()).is_ok()
    {
        target.insert("codexAppDreamSkinThemeConfig".to_string(), value.clone());
    }
    if let Some(value) = source
        .get("codexAppDreamSkinImagePath")
        .and_then(Value::as_str)
    {
        target.insert(
            "codexAppDreamSkinImagePath".to_string(),
            Value::String(value.trim().to_string()),
        );
    }
    if let Some(value) = source.get("codexGoalsEnabled").and_then(Value::as_bool) {
        target.insert("codexGoalsEnabled".to_string(), Value::Bool(value));
    }
    merge_bool_setting(target, source, "weixinConnectEnabled");
    for key in [
        "weixinConnectBaseUrl",
        "weixinConnectToken",
        "weixinConnectAccountId",
        "weixinConnectAllowFrom",
        "weixinConnectRouteTag",
        "weixinConnectWorkDir",
        "weixinConnectModel",
        "weixinConnectSandbox",
        "weixinConnectCodexPath",
    ] {
        if let Some(value) = source.get(key).and_then(Value::as_str) {
            target.insert(key.to_string(), Value::String(value.trim().to_string()));
        }
    }
    if let Some(value) = source.get("launchMode").and_then(Value::as_str) {
        if matches!(value, "patch" | "relay") {
            target.insert("launchMode".to_string(), Value::String(value.to_string()));
        }
    }
    if let Some(value) = source.get("relayBaseUrl").and_then(Value::as_str) {
        target.insert("relayBaseUrl".to_string(), Value::String(value.to_string()));
    }
    if let Some(value) = source.get("relayApiKey").and_then(Value::as_str) {
        target.insert("relayApiKey".to_string(), Value::String(value.to_string()));
    }
    if let Some(value) = source.get("relayProfiles").and_then(Value::as_array) {
        let mut profiles = serde_json::from_value::<Vec<RelayProfile>>(Value::Array(value.clone()))
            .unwrap_or_default();
        preserve_official_mix_bearer_tokens(&mut profiles, target);
        target.insert(
            "relayProfiles".to_string(),
            serde_json::to_value(profiles).unwrap_or_else(|_| Value::Array(Vec::new())),
        );
    }
    if let Some(value) = source
        .get("relayCommonConfigContents")
        .and_then(Value::as_str)
    {
        target.insert(
            "relayCommonConfigContents".to_string(),
            Value::String(value.to_string()),
        );
    }
    if let Some(value) = source
        .get("relayContextConfigContents")
        .and_then(Value::as_str)
    {
        target.insert(
            "relayContextConfigContents".to_string(),
            Value::String(value.to_string()),
        );
    }
    if let Some(value) = source.get("activeRelayId").and_then(Value::as_str) {
        target.insert(
            "activeRelayId".to_string(),
            Value::String(value.to_string()),
        );
    }
    if let Some(value) = source
        .get("aggregateRelayProfiles")
        .and_then(Value::as_array)
    {
        target.insert(
            "aggregateRelayProfiles".to_string(),
            Value::Array(value.clone()),
        );
    }
    if let Some(value) = source.get("activeAggregateRelayId").and_then(Value::as_str) {
        target.insert(
            "activeAggregateRelayId".to_string(),
            Value::String(value.to_string()),
        );
    }
    if let Some(value) = source.get("relayTestModel").and_then(Value::as_str) {
        target.insert(
            "relayTestModel".to_string(),
            Value::String(if value.trim().is_empty() {
                default_relay_test_model()
            } else {
                value.trim().to_string()
            }),
        );
    }
}

fn merge_bool_setting(target: &mut Map<String, Value>, source: &Map<String, Value>, key: &str) {
    if let Some(value) = source.get(key).and_then(Value::as_bool) {
        target.insert(key.to_string(), Value::Bool(value));
    }
}

fn preserve_official_mix_bearer_tokens(
    profiles: &mut [RelayProfile],
    previous: &Map<String, Value>,
) {
    let previous_tokens = previous
        .get("relayProfiles")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| serde_json::from_value::<RelayProfile>(value.clone()).ok())
        .filter_map(|profile| {
            if profile.relay_mode != RelayMode::Official || !profile.official_mix_api_key {
                return None;
            }
            let token = experimental_bearer_token_from_config_text(&profile.config_contents)?;
            Some((profile.id, token))
        })
        .collect::<HashMap<_, _>>();

    for profile in profiles {
        if profile.relay_mode != RelayMode::Official || !profile.official_mix_api_key {
            continue;
        }
        if experimental_bearer_token_from_config_text(&profile.config_contents).is_some() {
            continue;
        }
        let token = if profile.api_key.trim().is_empty() {
            previous_tokens.get(&profile.id).cloned()
        } else {
            Some(profile.api_key.trim().to_string())
        };
        let Some(token) = token else {
            continue;
        };
        profile.config_contents =
            set_or_replace_experimental_bearer_token(&profile.config_contents, &token);
    }
}

fn set_or_replace_experimental_bearer_token(contents: &str, token: &str) -> String {
    let mut doc = parse_toml_document(contents).unwrap_or_else(|_| DocumentMut::new());
    let session_provider_id =
        active_provider_id(&doc).unwrap_or_else(|| "codex-plus-relay".to_string());
    let transport_provider_id = if session_provider_id == "openai" {
        "custom"
    } else {
        session_provider_id.as_str()
    };
    doc["model_provider"] = toml_edit::value(session_provider_id.as_str());
    doc["model_providers"][transport_provider_id]["experimental_bearer_token"] =
        toml_edit::value(token.trim());
    ensure_text_newline(doc.to_string())
}

fn ensure_text_newline(mut value: String) -> String {
    if !value.is_empty() && !value.ends_with('\n') {
        value.push('\n');
    }
    value
}

fn experimental_bearer_token_from_config_text(contents: &str) -> Option<String> {
    let doc = parse_toml_document(contents).ok()?;
    let provider_id = active_provider_id(&doc)?;
    let token_from = |provider_id: &str| {
        doc.get("model_providers")
            .and_then(Item::as_table)
            .and_then(|providers| providers.get(provider_id))
            .and_then(Item::as_table)
            .and_then(|provider| provider.get("experimental_bearer_token"))
            .and_then(Item::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    };
    token_from(&provider_id).or_else(|| {
        (provider_id == "openai")
            .then(|| token_from("custom"))
            .flatten()
    })
}

fn active_provider_id(doc: &DocumentMut) -> Option<String> {
    doc.get("model_provider")
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .map(ToString::to_string)
}

fn parse_toml_document(contents: &str) -> anyhow::Result<DocumentMut> {
    let contents = contents.trim_start_matches('\u{feff}');
    if contents.trim().is_empty() {
        Ok(DocumentMut::new())
    } else {
        contents
            .parse::<DocumentMut>()
            .map_err(|error| anyhow::anyhow!("config.toml TOML 解析失败：{error}"))
    }
}

fn settings_to_object(settings: &BackendSettings) -> Map<String, Value> {
    match serde_json::to_value(settings).unwrap_or_else(|_| Value::Object(Map::new())) {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn normalize_settings_config_sections(mut settings: BackendSettings) -> BackendSettings {
    settings.ccs_db_path = settings.ccs_db_path.trim().to_string();
    let (common, extracted_context) =
        split_context_config_sections(&settings.relay_common_config_contents);
    let context = join_config_sections(&[
        settings.relay_context_config_contents.as_str(),
        extracted_context.as_str(),
    ]);
    settings.relay_common_config_contents = crate::relay_config::normalize_config_text(&common);
    settings.relay_context_config_contents = crate::relay_config::strip_legacy_skill_tables(
        &crate::relay_config::normalize_config_text(&context),
    );
    for profile in &mut settings.relay_profiles {
        let _ = crate::relay_config::normalize_relay_profile_for_storage(profile);
    }
    normalize_aggregate_member_order(&mut settings);
    settings.codex_app_image_overlay_opacity =
        clamp_image_overlay_opacity(settings.codex_app_image_overlay_opacity);
    settings.codex_app_image_overlay_fit_mode =
        normalize_image_overlay_fit_mode(&settings.codex_app_image_overlay_fit_mode);
    settings.codex_app_dream_skin_theme =
        normalize_dream_skin_theme(&settings.codex_app_dream_skin_theme);
    if settings.codex_app_dream_skin_theme_config == DreamSkinThemeConfig::default()
        && settings.codex_app_dream_skin_theme != default_dream_skin_theme()
    {
        settings.codex_app_dream_skin_theme_config.id = settings.codex_app_dream_skin_theme.clone();
    }
    settings.codex_app_dream_skin_image_path =
        settings.codex_app_dream_skin_image_path.trim().to_string();
    settings.codex_app_stepwise_base_url = settings
        .codex_app_stepwise_base_url
        .trim()
        .trim_end_matches('/')
        .to_string();
    settings.codex_app_stepwise_api_key = settings.codex_app_stepwise_api_key.trim().to_string();
    settings.codex_app_stepwise_api_key_env =
        if settings.codex_app_stepwise_api_key_env.trim().is_empty() {
            default_stepwise_api_key_env()
        } else {
            settings.codex_app_stepwise_api_key_env.trim().to_string()
        };
    settings.codex_app_stepwise_protocol =
        normalize_stepwise_protocol(&settings.codex_app_stepwise_protocol);
    settings.codex_app_stepwise_model = settings.codex_app_stepwise_model.trim().to_string();
    settings.weixin_connect_base_url = settings
        .weixin_connect_base_url
        .trim()
        .trim_end_matches('/')
        .to_string();
    if settings.weixin_connect_base_url.is_empty() {
        settings.weixin_connect_base_url = default_weixin_connect_base_url();
    }
    settings.weixin_connect_token = settings.weixin_connect_token.trim().to_string();
    settings.weixin_connect_account_id = settings.weixin_connect_account_id.trim().to_string();
    settings.weixin_connect_allow_from = settings.weixin_connect_allow_from.trim().to_string();
    settings.weixin_connect_route_tag = settings.weixin_connect_route_tag.trim().to_string();
    settings.weixin_connect_work_dir = settings.weixin_connect_work_dir.trim().to_string();
    settings.weixin_connect_model = settings.weixin_connect_model.trim().to_string();
    settings.weixin_connect_sandbox = match settings.weixin_connect_sandbox.trim() {
        "workspace-write" => "workspace-write",
        "danger-full-access" => "danger-full-access",
        _ => "read-only",
    }
    .to_string();
    settings.weixin_connect_codex_path = settings.weixin_connect_codex_path.trim().to_string();
    settings.codex_app_stepwise_max_items =
        clamp_stepwise_max_items(settings.codex_app_stepwise_max_items);
    settings.codex_app_stepwise_max_input_chars =
        clamp_stepwise_max_input_chars(settings.codex_app_stepwise_max_input_chars);
    settings.codex_app_stepwise_max_output_tokens =
        clamp_stepwise_max_output_tokens(settings.codex_app_stepwise_max_output_tokens);
    settings.codex_app_stepwise_timeout_ms =
        clamp_stepwise_timeout_ms(settings.codex_app_stepwise_timeout_ms);
    settings
}

fn normalize_aggregate_member_order(settings: &mut BackendSettings) {
    for aggregate in &mut settings.aggregate_relay_profiles {
        let mut stored_members = std::mem::take(&mut aggregate.members);
        let mut ordered_members = Vec::with_capacity(stored_members.len());
        for relay in &settings.relay_profiles {
            if let Some(index) = stored_members
                .iter()
                .position(|member| member.relay_id == relay.id)
            {
                ordered_members.push(stored_members.remove(index));
            }
        }
        // Keep unknown members so the existing validation path can report them
        // instead of silently discarding a malformed settings entry.
        ordered_members.extend(stored_members);
        aggregate.members = ordered_members;
    }
}

fn split_context_config_sections(config: &str) -> (String, String) {
    let mut common = Vec::new();
    let mut context = Vec::new();
    let mut in_context_table = false;

    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_context_table = is_context_table_header(trimmed);
        }
        if in_context_table {
            context.push(line);
        } else {
            common.push(line);
        }
    }

    (
        normalize_text_config(common.join("\n")),
        normalize_text_config(context.join("\n")),
    )
}

fn is_context_table_header(header: &str) -> bool {
    header.starts_with("[mcp_servers.")
        || header.starts_with("[skills.")
        || header.starts_with("[plugins.")
}

fn join_config_sections(sections: &[&str]) -> String {
    let joined = sections
        .iter()
        .map(|section| section.trim())
        .filter(|section| !section.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    normalize_text_config(joined)
}

fn normalize_text_config(contents: String) -> String {
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    let temp_path = temp_path_for(path);
    fs::write(&temp_path, bytes)
        .with_context(|| format!("failed to write temp file {}", temp_path.display()))?;
    if let Err(error) = replace_file(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error).with_context(|| {
            format!(
                "failed to replace {} with {}",
                path.display(),
                temp_path.display()
            )
        });
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> anyhow::Result<()> {
    fs::rename(source, target)?;
    Ok(())
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> anyhow::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    use windows::core::PCWSTR;

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )?;
    }
    Ok(())
}

fn temp_path_for(path: &Path) -> PathBuf {
    let mut temp_path = path.to_path_buf();
    let extension = path.extension().and_then(|value| value.to_str());
    temp_path.set_extension(match extension {
        Some(extension) => format!("{extension}.tmp"),
        None => "tmp".to_string(),
    });
    temp_path
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "codex-plus-core-settings-test-{}-{}",
            std::process::id(),
            NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn atomic_write_replaces_existing_file_and_removes_temp_file() {
        let dir = temp_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, b"old").unwrap();

        atomic_write(&path, b"new").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(!dir.join("settings.json.tmp").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn settings_default_matches_expected_behavior() {
        let settings = BackendSettings::default();
        assert!(!settings.provider_sync_enabled);
        assert!(settings.relay_profiles_enabled);
        assert!(settings.enhancements_enabled);
        assert!(settings.codex_app_plugin_marketplace_unlock);
        assert!(!settings.codex_app_thread_id_badge);
        assert!(settings.codex_app_force_chinese_locale);
        assert!(!settings.codex_goals_enabled);
        assert!(settings.codex_app_path.is_empty());
        assert!(settings.codex_extra_args.is_empty());
        assert_eq!(
            settings.zed_remote_open_strategy,
            ZedOpenStrategy::AddToFocusedWorkspace
        );
        assert!(settings.zed_remote_project_registry_enabled);
        assert!(!settings.zed_remote_sync_to_zed_settings);
        assert!(settings.codex_app_native_menu_localization);
        assert_eq!(settings.launch_mode, LaunchMode::Patch);
        assert_eq!(settings.relay_base_url, default_relay_base_url());
        assert!(settings.relay_api_key.is_empty());
        assert_eq!(settings.relay_profiles[0].relay_mode, RelayMode::Official);
        assert!(settings.relay_common_config_contents.is_empty());
        assert_eq!(settings.relay_test_model, default_relay_test_model());
        assert!(!settings.codex_app_stepwise_enabled);
        assert!(!settings.codex_app_stepwise_direct_send);
        assert!(settings.codex_app_stepwise_base_url.is_empty());
        assert!(settings.codex_app_stepwise_api_key.is_empty());
        assert_eq!(
            settings.codex_app_stepwise_api_key_env,
            "CODEX_STEPWISE_API_KEY"
        );
        assert_eq!(settings.codex_app_stepwise_protocol, "chat_completions");
        assert!(settings.codex_app_stepwise_model.is_empty());
        assert_eq!(settings.codex_app_stepwise_max_items, 6);
        assert_eq!(settings.codex_app_stepwise_max_input_chars, 6000);
        assert_eq!(settings.codex_app_stepwise_max_output_tokens, 500);
        assert_eq!(settings.codex_app_stepwise_timeout_ms, 8000);
        assert!(!settings.weixin_connect_enabled);
        assert_eq!(
            settings.weixin_connect_base_url,
            crate::connect::DEFAULT_WEIXIN_BASE_URL
        );
        assert!(settings.weixin_connect_token.is_empty());
        assert_eq!(settings.weixin_connect_sandbox, "read-only");
    }

    #[test]
    fn settings_deserialize_normalizes_stepwise_protocol_and_supports_legacy_missing_field() {
        let defaults: BackendSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(defaults.codex_app_stepwise_protocol, "chat_completions");

        for protocol in [
            "chat_completions",
            "responses",
            "anthropic_messages",
            "auto",
        ] {
            let settings: BackendSettings = serde_json::from_value(json!({
                "codexAppStepwiseProtocol": format!(" {protocol} ")
            }))
            .unwrap();
            assert_eq!(settings.codex_app_stepwise_protocol, protocol);
        }

        let invalid: BackendSettings = serde_json::from_value(json!({
            "codexAppStepwiseProtocol": "unsupported"
        }))
        .unwrap();
        assert_eq!(invalid.codex_app_stepwise_protocol, "chat_completions");
    }

    #[test]
    fn settings_deserialize_ignores_removed_cli_wrapper_keys() {
        let settings: BackendSettings = serde_json::from_str(
            r#"{"codexAppPath":"C:\\Portable\\Codex\\app","providerSyncEnabled":true,"codexGoalsEnabled":true,"cliWrapperEnabled":true,"cliWrapperBaseUrl":"https://example.test","cliWrapperApiKey":"sk-test","cliWrapperApiKeyEnv":""}"#,
        )
        .unwrap();
        assert_eq!(settings.codex_app_path, r"C:\Portable\Codex\app");
        assert!(settings.provider_sync_enabled);
        assert!(settings.codex_goals_enabled);
        assert_eq!(settings.relay_base_url, default_relay_base_url());
        assert!(settings.codex_extra_args.is_empty());
        let saved = serde_json::to_value(&settings).unwrap();
        assert!(saved.get("cliWrapperEnabled").is_none());
        assert!(saved.get("cliWrapperBaseUrl").is_none());
        assert!(saved.get("cliWrapperApiKey").is_none());
        assert!(saved.get("cliWrapperApiKeyEnv").is_none());
    }

    #[test]
    fn settings_deserialize_keeps_plugin_marketplace_unlock_switch() {
        let settings: BackendSettings = serde_json::from_str(
            r#"{
                "codexAppPluginMarketplaceUnlock": true,
                "codexAppPluginAutoExpand": false
            }"#,
        )
        .unwrap();

        assert!(settings.codex_app_plugin_marketplace_unlock);
        let saved = serde_json::to_value(&settings).unwrap();
        assert!(saved.get("codexAppPluginAutoExpand").is_none());

        let legacy_settings: BackendSettings = serde_json::from_str(
            r#"{
                "codexAppForcePluginInstall": false
            }"#,
        )
        .unwrap();

        assert!(legacy_settings.codex_app_plugin_marketplace_unlock);
    }

    #[test]
    fn settings_deserialize_reads_codex_extra_args() {
        let settings: BackendSettings = serde_json::from_str(
            r#"{"codexExtraArgs":["--force_high_performance_gpu"," --ignored-trimmed-by-ui "]}"#,
        )
        .unwrap();

        assert_eq!(
            settings.codex_extra_args,
            vec![
                "--force_high_performance_gpu".to_string(),
                " --ignored-trimmed-by-ui ".to_string(),
            ]
        );
    }

    #[test]
    fn relay_profile_official_mix_api_key_defaults_to_false() {
        let profile: RelayProfile =
            serde_json::from_str(r#"{"id":"official","name":"官方","relayMode":"official"}"#)
                .unwrap();

        assert_eq!(profile.relay_mode, RelayMode::Official);
        assert!(!profile.official_mix_api_key);
        assert!(!profile.hide_official_usage_alert);
        assert!(profile.test_model.is_empty());
    }

    #[test]
    fn relay_profile_reasoning_policy_defaults_and_round_trips() {
        let legacy: RelayProfile =
            serde_json::from_str(r#"{"id":"legacy","name":"Legacy"}"#).unwrap();
        assert_eq!(
            legacy.responses_reasoning_policy,
            ResponsesReasoningPolicy::Passthrough
        );

        for (raw, expected) in [
            ("passthrough", ResponsesReasoningPolicy::Passthrough),
            ("openAiOpaque", ResponsesReasoningPolicy::OpenAiOpaque),
            ("strip", ResponsesReasoningPolicy::Strip),
        ] {
            let profile: RelayProfile = serde_json::from_value(json!({
                "id": "relay",
                "name": "Relay",
                "responsesReasoningPolicy": raw
            }))
            .unwrap();
            assert_eq!(profile.responses_reasoning_policy, expected);
            assert_eq!(
                serde_json::to_value(profile).unwrap()["responsesReasoningPolicy"],
                raw
            );
        }
    }

    #[test]
    fn relay_profile_context_fields_default_to_empty() {
        let profile = RelayProfile::default();

        assert!(profile.use_common_config);
        assert!(profile.context_window.is_empty());
        assert!(profile.auto_compact_limit.is_empty());
        assert_eq!(profile.model_insert_mode, RelayModelInsertMode::Patch);
        assert!(profile.model_list.is_empty());
        assert!(profile.model_routes.is_empty());
        assert!(!profile.has_model_routes());
    }

    #[test]
    fn no_auth_relay_requires_protocol_proxy() {
        let settings = BackendSettings {
            relay_profiles: vec![RelayProfile {
                relay_mode: RelayMode::PureApi,
                no_auth: true,
                base_url: "https://relay.example.test/v1".to_string(),
                ..RelayProfile::default()
            }],
            ..BackendSettings::default()
        };

        assert!(settings.active_relay_uses_protocol_proxy());
    }

    #[test]
    fn relay_profile_model_routes_roundtrip_in_camel_case() {
        let profile: RelayProfile = serde_json::from_str(
            r#"{
                "id":"relay-a",
                "name":"供应商 A",
                "modelRoutes":[{
                    "model":"gpt-5.6-luna",
                    "targetRelayId":"relay-b",
                    "targetModel":"provider-luna"
                }]
            }"#,
        )
        .unwrap();

        assert!(profile.has_model_routes());
        assert_eq!(profile.model_routes[0].model, "gpt-5.6-luna");
        assert_eq!(profile.model_routes[0].target_relay_id, "relay-b");
        assert_eq!(profile.model_routes[0].target_model, "provider-luna");

        let saved = serde_json::to_value(profile).unwrap();
        assert_eq!(saved["modelRoutes"][0]["targetRelayId"], "relay-b");
        assert_eq!(saved["modelRoutes"][0]["targetModel"], "provider-luna");
    }

    // R08：wire 策略字段 serde round-trip；缺失字段默认 compatible，不改变旧 profile 行为。
    #[test]
    fn relay_profile_responses_wire_policy_roundtrip() {
        let missing: RelayProfile = serde_json::from_str(
            r#"{ "id":"relay-a", "name":"A" }"#,
        )
        .unwrap();
        assert_eq!(
            missing.responses_wire_policy,
            ResponsesWirePolicy::Compatible,
            "旧 profile 缺失新字段时保持既有兼容行为"
        );

        let passthrough: RelayProfile = serde_json::from_str(
            r#"{ "id":"relay-a", "name":"A", "responsesWirePolicy":"passthrough" }"#,
        )
        .unwrap();
        assert_eq!(passthrough.responses_wire_policy, ResponsesWirePolicy::Passthrough);

        let saved = serde_json::to_value(passthrough).unwrap();
        assert_eq!(saved["responsesWirePolicy"], "passthrough");
    }

    // Custom-as-Function 开关：缺失字段默认 false（旧 profile 行为不变），
    // true/false round-trip 一致，且字段始终序列化以便 CLI provider-update 识别。
    #[test]
    fn relay_profile_custom_tools_as_functions_roundtrip() {
        let missing: RelayProfile =
            serde_json::from_str(r#"{ "id":"relay-a", "name":"A" }"#).unwrap();
        assert!(
            !missing.custom_tools_as_functions,
            "旧 profile 缺失字段时适配器必须关闭"
        );

        let enabled: RelayProfile = serde_json::from_str(
            r#"{ "id":"relay-a", "name":"A", "customToolsAsFunctions":true }"#,
        )
        .unwrap();
        assert!(enabled.custom_tools_as_functions);
        let saved = serde_json::to_value(enabled).unwrap();
        assert_eq!(saved["customToolsAsFunctions"], true);

        let disabled: RelayProfile = serde_json::from_str(
            r#"{ "id":"relay-a", "name":"A", "customToolsAsFunctions":false }"#,
        )
        .unwrap();
        assert!(!disabled.custom_tools_as_functions);
        let saved_disabled = serde_json::to_value(disabled).unwrap();
        assert_eq!(saved_disabled["customToolsAsFunctions"], false);
    }

    #[test]
    fn relay_profile_native_interop_and_model_aliases_roundtrip() {
        let profile: RelayProfile = serde_json::from_value(json!({
            "id": "relay",
            "name": "Relay",
            "nativeAgentInterop": "on",
            "modelAliases": [{"alias": "luna", "model": "glm-5.3-flash"}]
        }))
        .unwrap();
        assert_eq!(profile.native_agent_interop, NativeAgentInterop::On);
        assert_eq!(profile.model_aliases.first().unwrap().alias, "luna");
        assert_eq!(
            profile.model_aliases.first().unwrap().model,
            "glm-5.3-flash"
        );
    }

    /// 旧版按供应商勾选上下文条目的 `contextSelection` / `contextSelectionInitialized`
    /// 已被上下文条目自身的 `enabled` 开关取代。历史 settings.json 里仍会带着这两个键，
    /// 反序列化必须容忍它们，否则老用户一升级配置就读不出来。
    #[test]
    fn relay_profile_context_fields_deserialize_from_camel_case() {
        let profile: RelayProfile = serde_json::from_str(
            r#"{
                "id":"relay-a",
                "name":"供应商 A",
                "contextSelection":{
                    "mcpServers":["context7"],
                    "skills":["writer"],
                    "plugins":["local"]
                },
                "contextSelectionInitialized":true,
                "useCommonConfig":false,
                "contextWindow":"200000",
                "autoCompactLimit":"160000",
                "modelInsertMode":"patch",
                "modelList":"qwen3-coder\ndeepseek-coder"
            }"#,
        )
        .unwrap();

        assert!(!profile.use_common_config);
        assert_eq!(profile.context_window, "200000");
        assert_eq!(profile.auto_compact_limit, "160000");
        assert_eq!(profile.model_insert_mode, RelayModelInsertMode::Patch);
        assert_eq!(profile.model_list, "qwen3-coder\ndeepseek-coder");
    }

    #[test]
    fn relay_profile_derived_fields_are_read_but_not_serialized() {
        let profile: RelayProfile = serde_json::from_str(
            r#"{
                "id":"relay-a",
                "name":"供应商 A",
                "model":"gpt-5.4",
                "baseUrl":"https://relay.example/v1",
                "apiKey":"sk-test",
                "configContents":"model = \"gpt-5.4\"\n",
                "authContents":"{\"OPENAI_API_KEY\":\"sk-test\"}"
            }"#,
        )
        .unwrap();

        assert_eq!(profile.model, "gpt-5.4");
        assert_eq!(profile.base_url, "https://relay.example/v1");
        assert_eq!(profile.api_key, "sk-test");

        let saved = serde_json::to_value(&profile).unwrap();
        assert!(saved.get("model").is_none());
        assert!(saved.get("baseUrl").is_none());
        assert!(saved.get("apiKey").is_none());
        assert_eq!(saved["configContents"], "model = \"gpt-5.4\"\n");
        assert_eq!(saved["authContents"], "{\"OPENAI_API_KEY\":\"sk-test\"}");
    }

    #[test]
    fn chat_protocol_profile_roundtrip_migrates_upstream_base_url_out_of_config() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));
        let settings = BackendSettings {
            relay_profiles: vec![RelayProfile {
                id: "relay-chat".to_string(),
                name: "DeepSeek".to_string(),
                protocol: RelayProtocol::ChatCompletions,
                relay_mode: RelayMode::PureApi,
                config_contents: r#"model = "deepseek-chat"
codex_plus_chat_base_url = "https://api.deepseek.com"
model_provider = "custom"

[model_providers.custom]
name = "custom"
wire_api = "responses"
requires_openai_auth = true
base_url = "http://127.0.0.1:57321/v1"
"#
                .to_string(),
                auth_contents: r#"{"OPENAI_API_KEY":"sk-test"}"#.to_string(),
                ..RelayProfile::default()
            }],
            active_relay_id: "relay-chat".to_string(),
            ..BackendSettings::default()
        };

        store.save(&settings).unwrap();
        let loaded = store.load().unwrap();
        let active = loaded.active_relay_profile();

        assert_eq!(active.protocol, RelayProtocol::ChatCompletions);
        assert_eq!(active.base_url, "https://api.deepseek.com");
        assert_eq!(active.upstream_base_url, "https://api.deepseek.com");
        assert_eq!(active.api_key, "sk-test");
        assert!(!active.config_contents.contains("codex_plus_chat_base_url"));

        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap())
                .unwrap();
        let profile = &saved["relayProfiles"][0];
        assert!(profile.get("baseUrl").is_none());
        assert_eq!(profile["upstreamBaseUrl"], "https://api.deepseek.com");
        assert!(profile.get("apiKey").is_none());
        assert!(
            !profile["configContents"]
                .as_str()
                .unwrap()
                .contains("codex_plus_chat_base_url")
        );
    }

    #[test]
    fn official_profile_without_mix_does_not_persist_api_config() {
        let settings = BackendSettings {
            relay_profiles: vec![RelayProfile {
                id: "official".to_string(),
                name: "官方".to_string(),
                relay_mode: RelayMode::Official,
                official_mix_api_key: false,
                hide_official_usage_alert: false,
                model: "gpt-5.5".to_string(),
                base_url: "https://relay.example/v1".to_string(),
                api_key: "sk-test".to_string(),
                config_contents: r#"model = "gpt-5.5"
model_provider = "custom"

[model_providers.custom]
requires_openai_auth = true
"#
                .to_string(),
                auth_contents: r#"{"OPENAI_API_KEY":"sk-test"}"#.to_string(),
                ..RelayProfile::default()
            }],
            active_relay_id: "official".to_string(),
            ..BackendSettings::default()
        };

        let value = settings_to_object(&normalize_settings_config_sections(settings));
        let profile = &value["relayProfiles"][0];
        assert_eq!(profile["relayMode"], "official");
        assert_eq!(profile["officialMixApiKey"], false);
        assert_eq!(profile["configContents"], "");
        assert_eq!(profile["authContents"], "");
        assert!(profile.get("model").is_none());
        assert!(profile.get("baseUrl").is_none());
        assert!(profile.get("apiKey").is_none());
    }

    #[test]
    fn official_mix_profile_keeps_key_in_config_not_auth() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));
        let settings = BackendSettings {
            relay_profiles: vec![RelayProfile {
                id: "official-mix".to_string(),
                name: "官方混入".to_string(),
                relay_mode: RelayMode::Official,
                official_mix_api_key: true,
                hide_official_usage_alert: false,
                model: "gpt-5.5".to_string(),
                base_url: "https://relay.example/v1".to_string(),
                api_key: "sk-mix".to_string(),
                config_contents: r#"model = "gpt-5.5"
model_provider = "custom"

[model_providers.custom]
requires_openai_auth = true
base_url = "https://relay.example/v1"
experimental_bearer_token = "sk-mix"
"#
                .to_string(),
                auth_contents: r#"{"OPENAI_API_KEY":"sk-mix","auth_mode":"chatgpt"}"#.to_string(),
                ..RelayProfile::default()
            }],
            active_relay_id: "official-mix".to_string(),
            ..BackendSettings::default()
        };

        store.save(&settings).unwrap();
        let loaded = store.load().unwrap();
        let profile = &loaded.relay_profiles[0];

        assert_eq!(profile.relay_mode, RelayMode::Official);
        assert!(profile.official_mix_api_key);
        assert_eq!(profile.api_key, "sk-mix");
        assert!(!profile.auth_contents.contains("OPENAI_API_KEY"));
        assert!(
            profile
                .config_contents
                .contains(r#"experimental_bearer_token = "sk-mix""#)
        );

        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap())
                .unwrap();
        assert!(saved["relayProfiles"][0].get("apiKey").is_none());
        assert!(
            !saved["relayProfiles"][0]["authContents"]
                .as_str()
                .unwrap()
                .contains("OPENAI_API_KEY")
        );
        assert!(
            saved["relayProfiles"][0]["configContents"]
                .as_str()
                .unwrap()
                .contains(r#"experimental_bearer_token = "sk-mix""#)
        );
    }

    #[test]
    fn settings_update_preserves_official_mix_key_when_payload_loses_it() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));
        store
            .save(&BackendSettings {
                relay_profiles: vec![RelayProfile {
                    id: "official-mix".to_string(),
                    name: "官方混入".to_string(),
                    relay_mode: RelayMode::Official,
                    official_mix_api_key: true,
                    hide_official_usage_alert: false,
                    config_contents: r#"model_provider = "custom"

[model_providers.other]
base_url = "https://other.example/v1"
experimental_bearer_token = "sk-other"

[model_providers.custom]
base_url = "https://relay.example/v1"
experimental_bearer_token = "sk-existing"
"#
                    .to_string(),
                    ..RelayProfile::default()
                }],
                active_relay_id: "official-mix".to_string(),
                ..BackendSettings::default()
            })
            .unwrap();

        let updated = store
            .update(json!({
                "relayProfiles": [{
                    "id": "official-mix",
                    "name": "官方混入",
                    "relayMode": "official",
                    "officialMixApiKey": true,
                    "configContents": "model_provider = \"custom\"\n\n[model_providers.other]\nbase_url = \"https://other.example/v1\"\nexperimental_bearer_token = \"sk-other\"\n\n[model_providers.custom]\nbase_url = \"https://relay.example/v1\"\nexperimental_bearer_token = \"\"\n",
                    "authContents": ""
                }],
                "activeRelayId": "official-mix"
            }))
            .unwrap();

        let profile = &updated.relay_profiles[0];
        assert_eq!(profile.api_key, "sk-existing");
        assert!(!profile.config_contents.contains("sk-other"));
        assert!(profile.config_contents.contains(
            r#"[model_providers.custom]
base_url = "https://relay.example/v1"
experimental_bearer_token = "sk-existing""#
        ));
    }

    #[test]
    fn official_mix_update_uses_api_key_when_config_token_missing() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "relayProfiles": [{
                    "id": "official-mix",
                    "name": "官方混入",
                    "relayMode": "official",
                    "officialMixApiKey": true,
                    "baseUrl": "https://relay.example/v1",
                    "apiKey": "sk-new",
                    "configContents": "model_provider = \"custom\"\n\n[model_providers.custom]\nbase_url = \"https://relay.example/v1\"\n",
                    "authContents": ""
                }],
                "activeRelayId": "official-mix"
            }))
            .unwrap();

        let profile = &updated.relay_profiles[0];
        assert_eq!(profile.api_key, "sk-new");
        assert!(
            profile
                .config_contents
                .contains(r#"experimental_bearer_token = "sk-new""#)
        );
        assert!(!profile.auth_contents.contains("OPENAI_API_KEY"));
    }

    #[test]
    fn settings_update_preserves_manual_official_mix_config_token() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "relayProfiles": [{
                    "id": "official-mix",
                    "name": "官方混入",
                    "relayMode": "official",
                    "officialMixApiKey": true,
                    "configContents": "model_provider = \"custom\"\n\n[model_providers.custom]\nbase_url = \"https://relay.example/v1\"\nexperimental_bearer_token = \"22222222222222222222222222222222222\"\n",
                    "authContents": ""
                }],
                "activeRelayId": "official-mix"
            }))
            .unwrap();

        let profile = &updated.relay_profiles[0];
        assert_eq!(profile.relay_mode, RelayMode::Official);
        assert!(profile.official_mix_api_key);
        assert_eq!(profile.api_key, "22222222222222222222222222222222222");
        assert!(
            profile
                .config_contents
                .contains(r#"experimental_bearer_token = "22222222222222222222222222222222222""#)
        );
        assert!(!profile.auth_contents.contains("OPENAI_API_KEY"));
    }

    #[test]
    fn settings_store_load_missing_file_returns_default() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        assert_eq!(store.load().unwrap(), BackendSettings::default());
    }

    #[test]
    fn settings_store_load_bad_json_returns_default() {
        let dir = temp_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, "{bad json").unwrap();
        let store = SettingsStore::new(path);

        assert_eq!(store.load().unwrap(), BackendSettings::default());
    }

    #[test]
    fn settings_store_save_load_roundtrip_uses_custom_path() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("nested").join("settings.json"));
        let settings = BackendSettings {
            provider_sync_enabled: true,
            codex_extra_args: vec!["--force_high_performance_gpu".to_string()],
            ccs_db_path: dir.join("cc-switch.db").to_string_lossy().to_string(),
            ..BackendSettings::default()
        };

        store.save(&settings).unwrap();

        assert_eq!(store.load().unwrap(), settings);
    }

    #[test]
    fn settings_store_model_routes_restore_target_credentials() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));
        let profile = |id: &str, base_url: &str, api_key: &str| RelayProfile {
            id: id.to_string(),
            name: id.to_string(),
            relay_mode: RelayMode::PureApi,
            upstream_base_url: base_url.to_string(),
            config_contents: format!(
                "model_provider = \"custom\"\n\n[model_providers.custom]\nname = \"custom\"\nwire_api = \"responses\"\nbase_url = \"{base_url}\"\n"
            ),
            auth_contents: format!(r#"{{"OPENAI_API_KEY":"{api_key}"}}"#),
            ..RelayProfile::default()
        };
        let mut source = profile("source", "https://source.example/v1", "sk-source");
        source.model_routes = vec![RelayModelRoute {
            model: "gpt-5.6-luna".to_string(),
            target_relay_id: "target".to_string(),
            target_model: String::new(),
            enabled: true,
            restore_at: None,
        }];
        let settings = BackendSettings {
            active_relay_id: "source".to_string(),
            relay_profiles: vec![
                source,
                profile("target", "https://target.example/v1", "sk-target"),
            ],
            ..BackendSettings::default()
        };

        store.save(&settings).unwrap();
        let loaded = store.load().unwrap();

        assert!(loaded.active_relay_uses_protocol_proxy());
        assert_eq!(
            loaded.relay_profiles[0].base_url,
            "https://source.example/v1"
        );
        assert_eq!(
            loaded.relay_profiles[1].base_url,
            "https://target.example/v1"
        );
        assert_eq!(loaded.relay_profiles[1].api_key, "sk-target");
        assert_eq!(
            loaded.relay_profiles[0].model_routes[0].target_relay_id,
            "target"
        );
    }

    #[test]
    fn settings_store_persists_and_normalizes_stepwise_protocol() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "codexAppStepwiseProtocol": "responses"
            }))
            .unwrap();
        assert_eq!(updated.codex_app_stepwise_protocol, "responses");
        assert_eq!(
            store.load().unwrap().codex_app_stepwise_protocol,
            "responses"
        );

        let invalid = store
            .update(json!({
                "codexAppStepwiseProtocol": "not-a-protocol"
            }))
            .unwrap();
        assert_eq!(invalid.codex_app_stepwise_protocol, "chat_completions");
        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(store.path).unwrap()).unwrap();
        assert_eq!(saved["codexAppStepwiseProtocol"], "chat_completions");
    }

    #[test]
    fn settings_store_save_load_roundtrip_preserves_aggregate_relay_settings() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));
        let settings = BackendSettings {
            relay_profiles: vec![
                RelayProfile {
                    id: "relay-a".to_string(),
                    name: "中转 A".to_string(),
                    ..RelayProfile::default()
                },
                RelayProfile {
                    id: "relay-b".to_string(),
                    name: "中转 B".to_string(),
                    ..RelayProfile::default()
                },
                RelayProfile {
                    id: "agg".to_string(),
                    name: "聚合".to_string(),
                    relay_mode: RelayMode::Aggregate,
                    ..RelayProfile::default()
                },
            ],
            active_relay_id: "agg".to_string(),
            aggregate_relay_profiles: vec![AggregateRelayProfile {
                id: "agg".to_string(),
                name: "聚合".to_string(),
                session_provider: RelaySessionProvider::Openai,
                code_mode_host: true,
                strategy: AggregateRelayStrategy::WeightedRoundRobin,
                members: vec![
                    AggregateRelayMember {
                        relay_id: "relay-a".to_string(),
                        weight: 1,
                    },
                    AggregateRelayMember {
                        relay_id: "relay-b".to_string(),
                        weight: 3,
                    },
                ],
            }],
            active_aggregate_relay_id: "agg".to_string(),
            ..BackendSettings::default()
        };

        store.save(&settings).unwrap();

        let loaded = store.load().unwrap();
        let expected = normalize_settings_config_sections(settings);
        let active_aggregate = loaded.active_aggregate_relay_profile().unwrap();
        assert_eq!(loaded, expected);
        assert_eq!(
            active_aggregate.strategy,
            AggregateRelayStrategy::WeightedRoundRobin
        );
        assert_eq!(active_aggregate.members[1].relay_id, "relay-b");
        assert_eq!(active_aggregate.members[1].weight, 3);
        assert_eq!(
            active_aggregate.session_provider,
            RelaySessionProvider::Openai
        );
        assert!(active_aggregate.code_mode_host);
        assert_eq!(
            loaded.active_relay_session_provider(),
            RelaySessionProvider::Openai
        );
        assert!(loaded.active_relay_uses_protocol_proxy());
    }

    #[test]
    fn active_aggregate_inherits_largest_member_context_and_paired_compaction_limit() {
        let settings = BackendSettings {
            relay_profiles: vec![
                RelayProfile {
                    id: "openai-account-relay".to_string(),
                    context_window: String::new(),
                    auto_compact_limit: String::new(),
                    ..RelayProfile::default()
                },
                RelayProfile {
                    id: "krill".to_string(),
                    model: "gpt-5.6-sol".to_string(),
                    context_window: "1000000".to_string(),
                    auto_compact_limit: "900000".to_string(),
                    model_list: "gpt-5.6-sol\ngpt-5.6-terra".to_string(),
                    model_windows: r#"{"gpt-5.6-sol":"1000000"}"#.to_string(),
                    ..RelayProfile::default()
                },
                RelayProfile {
                    id: "shuai-api".to_string(),
                    context_window: "272000".to_string(),
                    auto_compact_limit: "250000".to_string(),
                    ..RelayProfile::default()
                },
                RelayProfile {
                    id: "agg".to_string(),
                    relay_mode: RelayMode::Aggregate,
                    context_window: String::new(),
                    auto_compact_limit: String::new(),
                    ..RelayProfile::default()
                },
            ],
            active_relay_id: "agg".to_string(),
            active_aggregate_relay_id: "agg".to_string(),
            aggregate_relay_profiles: vec![AggregateRelayProfile {
                id: "agg".to_string(),
                name: "聚合".to_string(),
                session_provider: RelaySessionProvider::Custom,
                code_mode_host: false,
                strategy: AggregateRelayStrategy::PriorityFallback,
                members: vec![
                    AggregateRelayMember {
                        relay_id: "openai-account-relay".to_string(),
                        weight: 1,
                    },
                    AggregateRelayMember {
                        relay_id: "krill".to_string(),
                        weight: 1,
                    },
                    AggregateRelayMember {
                        relay_id: "shuai-api".to_string(),
                        weight: 1,
                    },
                ],
            }],
            ..BackendSettings::default()
        };

        let active = settings.active_relay_profile();
        assert_eq!(active.context_window, "1000000");
        assert_eq!(active.auto_compact_limit, "900000");
        assert_eq!(active.model, "gpt-5.6-sol");
        assert_eq!(active.model_list, "gpt-5.6-sol\ngpt-5.6-terra");
        assert_eq!(active.model_windows, r#"{"gpt-5.6-sol":"1000000"}"#);
        assert!(settings.relay_profiles[3].context_window.is_empty());
    }

    #[test]
    fn active_aggregate_explicit_context_and_compaction_limit_override_members() {
        let settings = BackendSettings {
            relay_profiles: vec![
                RelayProfile {
                    id: "member".to_string(),
                    context_window: "1000000".to_string(),
                    auto_compact_limit: "900000".to_string(),
                    ..RelayProfile::default()
                },
                RelayProfile {
                    id: "agg".to_string(),
                    relay_mode: RelayMode::Aggregate,
                    context_window: "500000".to_string(),
                    auto_compact_limit: "450000".to_string(),
                    ..RelayProfile::default()
                },
            ],
            active_relay_id: "agg".to_string(),
            active_aggregate_relay_id: "agg".to_string(),
            aggregate_relay_profiles: vec![AggregateRelayProfile {
                id: "agg".to_string(),
                name: "聚合".to_string(),
                session_provider: RelaySessionProvider::Custom,
                code_mode_host: false,
                strategy: AggregateRelayStrategy::PriorityFallback,
                members: vec![AggregateRelayMember {
                    relay_id: "member".to_string(),
                    weight: 1,
                }],
            }],
            ..BackendSettings::default()
        };

        let active = settings.active_relay_profile();
        assert_eq!(active.context_window, "500000");
        assert_eq!(active.auto_compact_limit, "450000");
    }

    #[test]
    fn active_aggregate_lists_the_union_of_member_models_in_member_order() {
        let settings = BackendSettings {
            relay_profiles: vec![
                RelayProfile {
                    id: "openai".to_string(),
                    model_list: "gpt-6-astra\ngpt-5.6-sol\ngpt-5.6-terra\ngpt-5.5\ngpt-5.4"
                        .to_string(),
                    ..RelayProfile::default()
                },
                RelayProfile {
                    id: "krill".to_string(),
                    model_list: "gpt-5.6-sol\ngpt-5.4\ngpt-5.5\ngpt-5.6-terra\ngpt-image-2"
                        .to_string(),
                    ..RelayProfile::default()
                },
                RelayProfile {
                    id: "shuai".to_string(),
                    model_list: "gpt-5.6-sol\ngpt-5.4\ngpt-5.6-terra\ngpt-5.5".to_string(),
                    ..RelayProfile::default()
                },
                RelayProfile {
                    id: "agg".to_string(),
                    relay_mode: RelayMode::Aggregate,
                    model_list: "gpt-5.6-sol".to_string(),
                    ..RelayProfile::default()
                },
            ],
            active_relay_id: "agg".to_string(),
            active_aggregate_relay_id: "agg".to_string(),
            aggregate_relay_profiles: vec![AggregateRelayProfile {
                id: "agg".to_string(),
                name: "聚合".to_string(),
                session_provider: RelaySessionProvider::Custom,
                code_mode_host: false,
                strategy: AggregateRelayStrategy::PriorityFallback,
                members: ["openai", "krill", "shuai"]
                    .into_iter()
                    .map(|relay_id| AggregateRelayMember {
                        relay_id: relay_id.to_string(),
                        weight: 1,
                    })
                    .collect(),
            }],
            ..BackendSettings::default()
        };

        let active = settings.active_relay_profile();
        assert_eq!(active.model, "gpt-5.6-sol");
        assert_eq!(
            active.model_list,
            "gpt-6-astra\ngpt-5.6-sol\ngpt-5.6-terra\ngpt-5.5\ngpt-5.4\ngpt-image-2"
        );
    }

    #[test]
    fn priority_fallback_strategy_uses_camel_case_json_and_roundtrips() {
        let serialized = serde_json::to_value(AggregateRelayStrategy::PriorityFallback).unwrap();
        assert_eq!(serialized, json!("priorityFallback"));
        assert_eq!(
            serde_json::from_value::<AggregateRelayStrategy>(serialized).unwrap(),
            AggregateRelayStrategy::PriorityFallback
        );
    }

    #[test]
    fn active_relay_session_provider_reads_standard_profile_config() {
        let mut settings = BackendSettings {
            relay_profiles: vec![RelayProfile {
                config_contents: "model_provider = \"openai\"\n".to_string(),
                ..RelayProfile::default()
            }],
            ..BackendSettings::default()
        };

        assert_eq!(
            settings.active_relay_session_provider(),
            RelaySessionProvider::Openai
        );

        settings.relay_profiles[0].config_contents = "model_provider = \"custom\"\n".to_string();
        assert_eq!(
            settings.active_relay_session_provider(),
            RelaySessionProvider::Custom
        );
    }

    #[test]
    fn settings_store_update_only_mutates_present_known_fields() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));
        let initial = BackendSettings {
            provider_sync_enabled: false,
            ..BackendSettings::default()
        };
        store.save(&initial).unwrap();

        let updated = store
            .update(json!({
            "providerSyncEnabled": true,
            "codexAppPath": "C:\\Portable\\Codex\\Codex.exe",
            "enhancementsEnabled": false,
            "codexAppSessionDelete": false,
            "codexAppConversationView": true,
            "codexAppThreadIdBadge": true,
            "codexAppNativeMenuLocalization": false,
            "codexAppServiceTierControls": true,
            "codexAppPetRealMouseLook": true,
            "codexGoalsEnabled": true,
            "relayBaseUrl": "https://relay.example.test/v1",
            "relayApiKey": "sk-relay",
            "codexExtraArgs": ["--force_high_performance_gpu", "", "  ", " --enable-gpu "],
            "unknownKey": "ignored"
            }))
            .unwrap();

        assert!(updated.provider_sync_enabled);
        assert_eq!(updated.codex_app_path, r"C:\Portable\Codex\Codex.exe");
        assert!(!updated.enhancements_enabled);
        assert!(!updated.codex_app_session_delete);
        assert!(updated.codex_app_conversation_view);
        assert!(updated.codex_app_thread_id_badge);
        assert!(!updated.codex_app_native_menu_localization);
        assert!(updated.codex_app_service_tier_controls);
        assert!(updated.codex_app_pet_real_mouse_look);
        assert!(updated.codex_goals_enabled);
        assert_eq!(updated.relay_base_url, "https://relay.example.test/v1");
        assert_eq!(updated.relay_api_key, "sk-relay");
        assert_eq!(
            updated.codex_extra_args,
            vec![
                "--force_high_performance_gpu".to_string(),
                "--enable-gpu".to_string(),
            ]
        );
        assert_eq!(store.load().unwrap(), updated);
    }

    #[test]
    fn settings_store_update_persists_image_overlay_settings() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "codexAppImageOverlayEnabled": true,
                "codexAppImageOverlayPath": "C:\\Users\\me\\Pictures\\overlay.png",
                "codexAppImageOverlayOpacity": 42,
                "codexAppImageOverlayFitMode": "fill"
            }))
            .unwrap();

        assert!(updated.codex_app_image_overlay_enabled);
        assert_eq!(
            updated.codex_app_image_overlay_path,
            r"C:\Users\me\Pictures\overlay.png"
        );
        assert_eq!(updated.codex_app_image_overlay_opacity, 42);
        assert_eq!(updated.codex_app_image_overlay_fit_mode, "fill");
        assert_eq!(store.load().unwrap(), updated);
    }

    #[test]
    fn settings_store_defaults_invalid_image_overlay_fit_mode_to_fit() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "codexAppImageOverlayFitMode": "unknown"
            }))
            .unwrap();

        assert_eq!(updated.codex_app_image_overlay_fit_mode, "fit");
    }

    #[test]
    fn settings_store_update_persists_dream_skin_settings() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "codexAppDreamSkinEnabled": true,
                "codexAppDreamSkinTheme": "miku",
                "codexAppDreamSkinImagePath": " C:\\Users\\me\\Pictures\\dream.webp "
            }))
            .unwrap();

        assert!(updated.codex_app_dream_skin_enabled);
        assert_eq!(updated.codex_app_dream_skin_theme, "miku");
        assert_eq!(
            updated.codex_app_dream_skin_image_path,
            r"C:\Users\me\Pictures\dream.webp"
        );
        assert_eq!(store.load().unwrap(), updated);
    }

    #[test]
    fn settings_store_defaults_invalid_dream_skin_theme_to_pink() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "codexAppDreamSkinTheme": "unknown"
            }))
            .unwrap();

        assert_eq!(updated.codex_app_dream_skin_theme, "pink");
    }

    #[test]
    fn legacy_market_theme_ids_resolve_to_layout_presets() {
        assert_eq!(
            resolve_dream_skin_style_preset("preset-cyber-neon", "dream-original"),
            "cyber-neon"
        );
        assert_eq!(
            resolve_dream_skin_style_preset("codex-snow-skin", ""),
            "codex-snow"
        );
        assert_eq!(
            resolve_dream_skin_style_preset("custom-theme", "dream-original"),
            "dream-original"
        );
    }

    #[test]
    fn settings_store_update_persists_stepwise_settings() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "codexAppStepwiseEnabled": true,
                "codexAppStepwiseDirectSend": true,
                "codexAppStepwiseBaseUrl": "https://api.example.test/v1/",
                "codexAppStepwiseApiKey": " sk-stepwise ",
                "codexAppStepwiseApiKeyEnv": "",
                "codexAppStepwiseModel": " stepwise-mini ",
                "codexAppStepwiseMaxItems": 12,
                "codexAppStepwiseMaxInputChars": 25000,
                "codexAppStepwiseMaxOutputTokens": 50,
                "codexAppStepwiseTimeoutMs": 70000
            }))
            .unwrap();

        assert!(updated.codex_app_stepwise_enabled);
        assert!(updated.codex_app_stepwise_direct_send);
        assert_eq!(
            updated.codex_app_stepwise_base_url,
            "https://api.example.test/v1"
        );
        assert_eq!(updated.codex_app_stepwise_api_key, "sk-stepwise");
        assert_eq!(
            updated.codex_app_stepwise_api_key_env,
            default_stepwise_api_key_env()
        );
        assert_eq!(updated.codex_app_stepwise_model, "stepwise-mini");
        assert_eq!(updated.codex_app_stepwise_max_items, 6);
        assert_eq!(updated.codex_app_stepwise_max_input_chars, 24000);
        assert_eq!(updated.codex_app_stepwise_max_output_tokens, 100);
        assert_eq!(updated.codex_app_stepwise_timeout_ms, 60000);
        assert_eq!(store.load().unwrap(), updated);
    }

    #[test]
    fn settings_store_update_persists_weixin_connect_settings() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "weixinConnectEnabled": true,
                "weixinConnectBaseUrl": "https://ilink.example.test/",
                "weixinConnectToken": " token ",
                "weixinConnectAccountId": " bot-1 ",
                "weixinConnectAllowFrom": " user@im.wechat ",
                "weixinConnectRouteTag": " route ",
                "weixinConnectWorkDir": " /workspace ",
                "weixinConnectModel": " gpt-test ",
                "weixinConnectSandbox": "workspace-write",
                "weixinConnectCodexPath": " /usr/local/bin/codex "
            }))
            .unwrap();

        assert!(updated.weixin_connect_enabled);
        assert_eq!(
            updated.weixin_connect_base_url,
            "https://ilink.example.test"
        );
        assert_eq!(updated.weixin_connect_token, "token");
        assert_eq!(updated.weixin_connect_account_id, "bot-1");
        assert_eq!(updated.weixin_connect_allow_from, "user@im.wechat");
        assert_eq!(updated.weixin_connect_route_tag, "route");
        assert_eq!(updated.weixin_connect_work_dir, "/workspace");
        assert_eq!(updated.weixin_connect_model, "gpt-test");
        assert_eq!(updated.weixin_connect_sandbox, "workspace-write");
        assert_eq!(updated.weixin_connect_codex_path, "/usr/local/bin/codex");
        assert_eq!(store.load().unwrap(), updated);
    }

    #[test]
    fn settings_store_update_persists_launch_mode() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store.update(json!({"launchMode": "relay"})).unwrap();
        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap())
                .unwrap();

        assert_eq!(updated.launch_mode, LaunchMode::Relay);
        assert_eq!(saved["launchMode"], json!("relay"));
    }

    #[test]
    fn settings_store_update_persists_relay_profiles_and_active_profile() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "relayProfiles": [
                    {
                        "id": "relay-a",
                        "name": "中转 A",
                        "baseUrl": "https://relay-a.example/v1",
                        "apiKey": "sk-a"
                    },
                    {
                        "id": "relay-b",
                        "name": "中转 B",
                        "baseUrl": "https://relay-b.example/v1",
                        "apiKey": "sk-b"
                    }
                ],
                "activeRelayId": "relay-b",
                "relayTestModel": "claude-sonnet-4"
            }))
            .unwrap();

        let active = updated.active_relay_profile();
        assert_eq!(updated.relay_profiles.len(), 2);
        assert_eq!(active.id, "relay-b");
        assert_eq!(active.name, "中转 B");
        assert_eq!(updated.relay_test_model, "claude-sonnet-4");

        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap())
                .unwrap();
        assert!(saved["relayProfiles"][1].get("baseUrl").is_none());
        assert!(saved["relayProfiles"][1].get("apiKey").is_none());
    }

    #[test]
    fn settings_store_update_does_not_persist_relay_profile_derived_fields() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "relayProfiles": [
                    {
                        "id": "relay-a",
                        "name": "供应商 A",
                        "model": "gpt-5.4",
                        "baseUrl": "https://relay.example/v1",
                        "apiKey": "sk-a",
                        "configContents": "model = \"gpt-5.4\"\n",
                        "authContents": "{\"OPENAI_API_KEY\":\"sk-a\"}"
                    }
                ],
                "activeRelayId": "relay-a"
            }))
            .unwrap();

        assert_eq!(updated.relay_profiles[0].id, "relay-a");
        assert_eq!(updated.relay_profiles[0].name, "供应商 A");

        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap())
                .unwrap();
        let saved_profile = &saved["relayProfiles"][0];
        assert!(saved_profile.get("model").is_none());
        assert!(saved_profile.get("baseUrl").is_none());
        assert!(saved_profile.get("apiKey").is_none());
        assert_eq!(saved_profile["configContents"], "model = \"gpt-5.4\"\n");
        assert_eq!(
            saved_profile["authContents"],
            "{\"OPENAI_API_KEY\":\"sk-a\"}"
        );
    }

    #[test]
    fn settings_store_update_moves_context_tables_out_of_common_config() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "relayCommonConfigContents": "[mcp_servers.context7]\ncommand = \"npx\"\n"
            }))
            .unwrap();

        assert!(updated.relay_common_config_contents.is_empty());
        assert_eq!(
            updated.relay_context_config_contents,
            "[mcp_servers.context7]\ncommand = \"npx\"\n"
        );
        assert_eq!(store.load().unwrap(), updated);
    }

    #[test]
    fn settings_store_update_extracts_context_config_from_common_config() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "relayCommonConfigContents": "model_reasoning_effort = \"high\"\n\n[mcp_servers.context7]\ncommand = \"npx\"\n\n[plugins.\"superpowers@openai-curated\"]\nenabled = true\n"
            }))
            .unwrap();

        assert_eq!(
            updated.relay_common_config_contents,
            "model_reasoning_effort = \"high\"\n"
        );
        assert!(
            updated
                .relay_context_config_contents
                .contains("[mcp_servers.context7]")
        );
        assert!(
            updated
                .relay_context_config_contents
                .contains("[plugins.\"superpowers@openai-curated\"]")
        );
        assert_eq!(store.load().unwrap(), updated);
    }

    #[test]
    fn settings_store_update_persists_aggregate_relay_profiles_and_active_id() {
        let dir = temp_dir();
        let store = SettingsStore::new(dir.join("settings.json"));

        let updated = store
            .update(json!({
                "relayProfiles": [
                    { "id": "relay-a", "name": "中转 A" },
                    { "id": "relay-b", "name": "中转 B" },
                    { "id": "agg", "name": "聚合", "relayMode": "aggregate" }
                ],
                "activeRelayId": "agg",
                "aggregateRelayProfiles": [
                    {
                        "id": "agg",
                        "name": "聚合",
                        "codeModeHost": true,
                        "strategy": "weightedRoundRobin",
                        "members": [
                            { "relayId": "relay-a", "weight": 1 },
                            { "relayId": "relay-b", "weight": 4 }
                        ]
                    }
                ],
                "activeAggregateRelayId": "agg"
            }))
            .unwrap();

        let active_aggregate = updated.active_aggregate_relay_profile().unwrap();
        assert_eq!(updated.active_relay_id, "agg");
        assert_eq!(updated.active_aggregate_relay_id, "agg");
        assert_eq!(
            active_aggregate.strategy,
            AggregateRelayStrategy::WeightedRoundRobin
        );
        assert_eq!(active_aggregate.members.len(), 2);
        assert_eq!(active_aggregate.members[1].relay_id, "relay-b");
        assert_eq!(active_aggregate.members[1].weight, 4);
        assert!(active_aggregate.code_mode_host);
        assert!(updated.active_relay_uses_protocol_proxy());
    }

    #[test]
    fn settings_load_reorders_selected_aggregate_members_to_match_relay_profiles() {
        let dir = temp_dir();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "relayProfiles": [
                    { "id": "openai-account-relay", "name": "OpenAI账号中转" },
                    { "id": "krill", "name": "krill" },
                    { "id": "shuai-api", "name": "帅api" },
                    { "id": "agg", "name": "聚合", "relayMode": "aggregate" }
                ],
                "activeRelayId": "agg",
                "aggregateRelayProfiles": [{
                    "id": "agg",
                    "name": "聚合",
                    "strategy": "priorityFallback",
                    "members": [
                        { "relayId": "krill", "weight": 2 },
                        { "relayId": "shuai-api", "weight": 3 },
                        { "relayId": "openai-account-relay", "weight": 1 }
                    ]
                }],
                "activeAggregateRelayId": "agg"
            }))
            .unwrap(),
        )
        .unwrap();

        let loaded = SettingsStore::new(path).load().unwrap();
        let member_ids = loaded.aggregate_relay_profiles[0]
            .members
            .iter()
            .map(|member| member.relay_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            member_ids,
            vec!["openai-account-relay", "krill", "shuai-api"]
        );
        assert_eq!(loaded.aggregate_relay_profiles[0].members[0].weight, 1);
    }

    #[test]
    fn active_relay_profile_uses_legacy_single_relay_when_profiles_are_default() {
        let settings = BackendSettings {
            relay_base_url: "https://legacy.example/v1".to_string(),
            relay_api_key: "sk-legacy".to_string(),
            ..BackendSettings::default()
        };

        let active = settings.active_relay_profile();

        assert_eq!(active.id, "default");
        assert_eq!(active.name, "默认中转");
        assert_eq!(active.base_url, "https://legacy.example/v1");
        assert_eq!(active.api_key, "sk-legacy");
        assert_eq!(active.relay_mode, RelayMode::MixedApi);
        assert!(active.official_mix_api_key);
    }

    #[test]
    fn settings_store_update_preserves_existing_unknown_fields() {
        let dir = temp_dir();
        let path = dir.join("settings.json");
        let store = SettingsStore::new(path.clone());
        std::fs::write(
            &path,
            r#"{"providerSyncEnabled":false,"customField":{"nested":true}}"#,
        )
        .unwrap();

        let updated = store
            .update(json!({
                "providerSyncEnabled": true
            }))
            .unwrap();
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        assert!(updated.provider_sync_enabled);
        assert_eq!(saved["providerSyncEnabled"], json!(true));
        assert_eq!(saved["codexExtraArgs"], Value::Null);
        assert_eq!(saved["customField"], json!({"nested": true}));
    }

    #[test]
    fn settings_store_update_removes_obsolete_setting_fields() {
        let dir = temp_dir();
        let path = dir.join("settings.json");
        let store = SettingsStore::new(path.clone());
        std::fs::write(
            &path,
            r#"{"providerSyncEnabled":false,"codexAppPluginAutoExpand":true,"computerUseGuardEnabled":true,"customField":1}"#,
        )
        .unwrap();

        store
            .update(json!({
                "providerSyncEnabled": true
            }))
            .unwrap();
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        assert!(saved.get("codexAppPluginAutoExpand").is_none());
        assert!(saved.get("computerUseGuardEnabled").is_none());
        assert_eq!(saved["customField"], json!(1));
    }

    #[test]
    fn settings_store_update_persists_codex_extra_args_and_preserves_unknown_fields() {
        let dir = temp_dir();
        let path = dir.join("settings.json");
        let store = SettingsStore::new(path.clone());
        std::fs::write(
            &path,
            r#"{"providerSyncEnabled":false,"customField":{"nested":true}}"#,
        )
        .unwrap();

        let updated = store
            .update(json!({
                "codexExtraArgs": ["--force_high_performance_gpu", "--enable-features=UseOzonePlatform"]
            }))
            .unwrap();
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        assert_eq!(
            updated.codex_extra_args,
            vec![
                "--force_high_performance_gpu".to_string(),
                "--enable-features=UseOzonePlatform".to_string(),
            ]
        );
        assert_eq!(
            saved["codexExtraArgs"],
            json!([
                "--force_high_performance_gpu",
                "--enable-features=UseOzonePlatform"
            ])
        );
        assert_eq!(saved["customField"], json!({"nested": true}));
    }

    #[test]
    fn settings_store_update_with_non_object_payload_does_not_write_file() {
        let dir = temp_dir();
        let path = dir.join("settings.json");
        let store = SettingsStore::new(path.clone());
        let original = r#"{"providerSyncEnabled":false,"customField":"keep me"}"#;
        std::fs::write(&path, original).unwrap();

        let updated = store.update(json!(null)).unwrap();

        assert!(!updated.provider_sync_enabled);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn old_model_route_json_defaults_to_enabled() {
        let route: RelayModelRoute = serde_json::from_value(json!({
            "model": "gpt-5.6-terra",
            "targetRelayId": "glm",
            "targetModel": "glm-5.3"
        }))
        .unwrap();

        assert!(route.enabled);
        assert!(route.restore_at.is_none());
        assert!(route.is_effectively_enabled_at(1));
    }

    #[test]
    fn model_route_set_supports_default_permanent_and_manual_enable() {
        let dir = temp_dir();
        let path = dir.join("settings.json");
        let store = SettingsStore::new(path.clone());
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "activeRelayId": "source",
                "relayProfiles": [
                    {
                        "id": "source",
                        "name": "Source",
                        "modelRoutes": [{
                            "model": "gpt-5.6-terra",
                            "targetRelayId": "target",
                            "targetModel": "glm-5.3",
                            "futureField": "keep"
                        }]
                    },
                    { "id": "target", "name": "Target" }
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        let disabled = store
            .model_route_set(
                &SetRelayModelRouteRequest {
                    id: "source".to_string(),
                    model: "gpt-5.6-terra".to_string(),
                    enabled: false,
                    restore_at: None,
                    duration_seconds: None,
                    permanent: false,
                },
                false,
            )
            .unwrap();
        assert!(!disabled.route.enabled);
        assert!(!disabled.route.permanent);
        assert!((17_999..=18_000).contains(&disabled.route.remaining_seconds));

        let permanent = store
            .model_route_set(
                &SetRelayModelRouteRequest {
                    id: "source".to_string(),
                    model: "gpt-5.6-terra".to_string(),
                    enabled: false,
                    restore_at: None,
                    duration_seconds: None,
                    permanent: true,
                },
                false,
            )
            .unwrap();
        assert!(permanent.route.permanent);
        assert!(permanent.route.restore_at.is_none());

        let enabled = store
            .model_route_set(
                &SetRelayModelRouteRequest {
                    id: "source".to_string(),
                    model: "gpt-5.6-terra".to_string(),
                    enabled: true,
                    restore_at: None,
                    duration_seconds: None,
                    permanent: false,
                },
                false,
            )
            .unwrap();
        assert!(enabled.route.enabled);
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            saved["relayProfiles"][0]["modelRoutes"][0]["futureField"],
            "keep"
        );
        assert_eq!(saved["relayProfiles"][0]["modelRoutes"][0]["enabled"], true);
        assert!(
            saved["relayProfiles"][0]["modelRoutes"][0]
                .get("restoreAt")
                .is_none()
        );
    }

    #[test]
    fn settings_save_preserves_authoritative_model_route_state() {
        let dir = temp_dir();
        let path = dir.join("settings.json");
        let store = SettingsStore::new(path);
        let mut settings = BackendSettings {
            active_relay_id: "source".to_string(),
            relay_profiles: vec![
                RelayProfile {
                    id: "source".to_string(),
                    name: "Source".to_string(),
                    relay_mode: RelayMode::PureApi,
                    upstream_base_url: "https://source.example/v1".to_string(),
                    config_contents: "model_provider = \"custom\"\n\n[model_providers.custom]\nname = \"custom\"\nwire_api = \"responses\"\nbase_url = \"https://source.example/v1\"\n".to_string(),
                    auth_contents: "{\"OPENAI_API_KEY\":\"sk-source\"}".to_string(),
                    model_routes: vec![RelayModelRoute {
                        model: "gpt-5.6-sol".to_string(),
                        target_relay_id: "target".to_string(),
                        target_model: String::new(),
                        enabled: false,
                        restore_at: Some(u64::MAX - 1),
                    }],
                    ..RelayProfile::default()
                },
                RelayProfile {
                    id: "target".to_string(),
                    name: "Target".to_string(),
                    relay_mode: RelayMode::PureApi,
                    upstream_base_url: "https://target.example/v1".to_string(),
                    config_contents: "model_provider = \"custom\"\n\n[model_providers.custom]\nname = \"custom\"\nwire_api = \"responses\"\nbase_url = \"https://target.example/v1\"\n".to_string(),
                    auth_contents: "{\"OPENAI_API_KEY\":\"sk-target\"}".to_string(),
                    ..RelayProfile::default()
                },
            ],
            ..BackendSettings::default()
        };
        store.save(&settings).unwrap();
        settings.relay_profiles[0].name = "Stale draft rename".to_string();
        settings.relay_profiles[0].model_routes[0].enabled = true;
        settings.relay_profiles[0].model_routes[0].restore_at = None;

        store.save(&settings).unwrap();
        let loaded = store.load().unwrap();
        let route = &loaded.relay_profiles[0].model_routes[0];
        assert_eq!(loaded.relay_profiles[0].name, "Stale draft rename");
        assert!(!route.enabled);
        assert_eq!(route.restore_at, Some(u64::MAX - 1));
    }
}
