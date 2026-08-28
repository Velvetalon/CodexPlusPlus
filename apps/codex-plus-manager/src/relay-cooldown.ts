export type RelayCooldownReason =
  | { type: "httpStatus"; statusCode: number }
  | { type: "transportFailure" };

export type RelayCooldownMemberStatus = {
  relayId: string;
  consecutiveFailures: number;
  failureThreshold: number;
  cooldownRemainingSeconds: number;
  lastCooldownReason: RelayCooldownReason | null;
};

export type RelayPriorityHighlights = {
  currentRelayId: string | null;
  nextRelayId: string | null;
};

/**
 * Mirror priority-fallback selection for the management UI. The current
 * provider is the first member that is not cooling down. The next provider is
 * the first other available member; if every other member is cooling down,
 * show the one that will recover first.
 */
export function relayPriorityHighlights(
  orderedRelayIds: string[],
  statuses: RelayCooldownMemberStatus[],
): RelayPriorityHighlights {
  const uniqueRelayIds = orderedRelayIds.filter(
    (relayId, index) => relayId && orderedRelayIds.indexOf(relayId) === index,
  );
  if (!uniqueRelayIds.length) return { currentRelayId: null, nextRelayId: null };

  const statusByRelayId = new Map(statuses.map((status) => [status.relayId, status] as const));
  const remainingSeconds = (relayId: string) =>
    Math.max(0, statusByRelayId.get(relayId)?.cooldownRemainingSeconds ?? 0);
  const earliestRecovery = (relayIds: string[]) =>
    relayIds.reduce<string | null>((selected, relayId) => {
      if (!selected) return relayId;
      return remainingSeconds(relayId) < remainingSeconds(selected) ? relayId : selected;
    }, null);

  const currentRelayId =
    uniqueRelayIds.find((relayId) => remainingSeconds(relayId) <= 0) ??
    earliestRecovery(uniqueRelayIds);
  const remainingRelayIds = uniqueRelayIds.filter((relayId) => relayId !== currentRelayId);
  const nextRelayId =
    remainingRelayIds.find((relayId) => remainingSeconds(relayId) <= 0) ??
    earliestRecovery(remainingRelayIds);

  return { currentRelayId, nextRelayId };
}

export function formatCooldownDuration(seconds: number): string {
  const whole = Math.max(0, Math.ceil(seconds));
  const hours = Math.floor(whole / 3600);
  const minutes = Math.floor((whole % 3600) / 60);
  const remainingSeconds = whole % 60;
  return hours > 0
    ? `${hours}:${String(minutes).padStart(2, "0")}:${String(remainingSeconds).padStart(2, "0")}`
    : `${minutes}:${String(remainingSeconds).padStart(2, "0")}`;
}

export function relayCooldownReasonLabel(
  reason: RelayCooldownReason | null,
  transportFailureLabel: string,
): string {
  if (!reason) return "";
  return reason.type === "httpStatus" ? `HTTP ${reason.statusCode}` : transportFailureLabel;
}
