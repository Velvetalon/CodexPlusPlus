export type AggregateOrderMember = {
  profileId: string;
};

/**
 * Keep only selected members, but project their priority onto the same order
 * users see in the relay profile list. An empty candidate list is used while
 * hydrating legacy payloads, so preserve the stored order in that case.
 */
export function orderAggregateMembersByCandidates<T extends AggregateOrderMember>(
  members: T[],
  candidateIds: string[],
): T[] {
  if (!candidateIds.length) return members;
  const selectedById = new Map(members.map((member) => [member.profileId, member] as const));
  return candidateIds.flatMap((candidateId) => {
    const member = selectedById.get(candidateId);
    return member ? [member] : [];
  });
}
