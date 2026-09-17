# ADR 0008 · Secrets, deployment, and the boundary around an unauthenticated prosperod

- **Status:** accepted
- **Date:** 2026-09-13
- **Revisited by:** [ADR 0013](0013-ariel-authenticates-to-prosperod.md), after prosperod gained API authentication
- **Source:** `docs/superpowers/specs/2026-07-03-ariel-chat-bridge-design.md` (caliban-ai umbrella workspace) §IdP federation, open question 5; issue #5

## Context

Ariel holds a Discord bot token now, and later Slack and Teams credentials. It is
also a privileged client of two services that, in the home cluster, check nothing:

- **prosperod's HTTP API has no authentication** (caliban-ai/prospero#2). Its
  Service is a ClusterIP on port 7878 with no NetworkPolicy, so any pod in any
  namespace can spawn and kill agents, including agent sandboxes, which run in
  prosperod's own `caliban` namespace. prosperod's service account can create,
  change and delete `CalibanTask` and `Workspace` resources, so reaching the port
  carries that authority. Its dashboard is published at `prospero.hexadecimate.net`
  through a Traefik route that admits only client addresses in `192.168.1.0/24`
  and has no authentication.
- **gonzalod runs with auth off.** It supports namespace-scoped bearer-token
  principals (gonzalo ADR 0015), but its chart can only set one plaintext token
  from values, and the cluster sets none. Once Ariel keeps role grants in gonzalo
  ([ADR 0003](0003-no-state-of-its-own.md)), any pod that can reach gonzalod could
  grant itself any Ariel role.

Ariel's two-key authorization only means something if Ariel is the gated path to
prosperod's mutating routes, and if the grants it reads cannot be forged.

The cluster's existing conventions constrain the answer:

- **GitOps.** A caliban-ai release reaches the cluster in three steps: the component
  image, then the published chart in `caliban-ai/helm-charts`, then a version pin in
  the private `johnford2002/helm-charts` `caliban-system` chart, which Argo CD syncs.
  Nobody runs `helm upgrade` by hand.
- **Secrets in git are SealedSecrets.** prosperod's session-plane token is a
  SealedSecret mounted as a file.
- **k3s enforces NetworkPolicy.** Private charts use an ingress policy that denies
  everything except Traefik; egress is not restricted anywhere yet.
- **Charts must pass kube-linter.** Every container sets CPU and memory requests and
  limits. App namespaces enforce baseline pod security.

Alternatives weighed:

- **Secret source:** External Secrets Operator with a vault backend (adds a platform
  component the cluster does not run), environment variables left to each deployer
  (leaves the home rollout undecided), or gonzalo records (rejected outright: gonzalo
  keeps revision history, and its git substrate commits every write).
- **Discord transport:** Discord's HTTP interactions endpoint needs a public HTTPS
  route and still needs a Gateway connection later for thread replies (#7).
- **Fencing prosperod:** admitting every pod in `caliban` re-admits agent sandboxes,
  and adding basic auth to the dashboard puts a login on the operator's own tool.
- **gonzalod identity:** sending a token while leaving auth off lets identity ship on
  forgeable grants; a NetworkPolicy alone cannot exclude agents, which need gonzalod
  for memory.

## Decision

### Secrets

- **Every Ariel credential is a SealedSecret** committed to the private
  `caliban-system` chart in namespace `caliban`: one Secret, `ariel-credentials`,
  with `discord-token` and `gonzalo-token` keys, and later platform keys.
- **Ariel's chart takes only references**, a Secret name and key per credential. It
  mounts each as a file read through `ARIEL_DISCORD_TOKEN_FILE` and
  `ARIEL_GONZALO_TOKEN_FILE`. An unreadable file stops `arield` at startup. Rotation
  means resealing and restarting the pod.
- **Secrets are never stored as gonzalo records, never placed in chart values, and
  never logged.**
- Ariel needs no Kubernetes API access: the pod mounts no service-account token and
  has no RBAC.

### Deployment

- **Image:** `ghcr.io/caliban-ai/ariel`, built and released the way prospero's is: a
  Rust 1.95 build stage, a slim Debian runtime, non-root uid 10001, and multi-arch
  images tagged by version and `sha-`.
- **Charts:** a generic `ariel` chart in `caliban-ai/helm-charts`, added to the
  umbrella but disabled by default, since without a bot token it never becomes
  Ready. The private `caliban-system` chart pins it, supplies the SealedSecret and
  values, and rolls it out when the pin merges.
- **Workload:** a Deployment with **one replica and the `Recreate` strategy**. Two
  Gateway sessions on one bot token would answer every command twice, so a rollout
  accepts a few seconds of downtime instead. CPU and memory requests and limits are
  set. The container runs non-root with all capabilities dropped and a read-only
  root filesystem, which satisfies baseline pod security. `arield` serves `/healthz`
  for probes.
- **No ingress.** Ariel receives Discord events over an outbound Gateway
  connection, so it has no Service exposed outside the cluster, no route and no
  public hostname. A Teams backend, which can only receive over public HTTPS, will
  add a route when it arrives.

### Network boundary

- **prosperod:** an ingress NetworkPolicy, optional in the public prospero chart
  (`networkPolicy.enabled`, a configurable list of allowed clients) and enabled in
  the private deploy, admits port 7878 only from Traefik in `kube-system` and from
  pods labelled `app.kubernetes.io/name: ariel`. Egress is unchanged.
- **Ariel:** an ingress NetworkPolicy admits only the health port. Egress stays open,
  matching the rest of the cluster; Discord's endpoints are not a fixed set.
- **On first rollout,** confirm that kubelet probes and `kubectl port-forward` still
  reach both pods, and admit the node addresses if they do not.

### The bypass threat

Every path to prosperod's mutating routes that does not pass through Ariel's
authorization, and its disposition:

| Path | Disposition |
|---|---|
| Any pod calling prosperod on 7878 | **Closed** by the allow-list |
| An agent sandbox in `caliban` calling prosperod | **Closed**: agents are not on the allow-list |
| A person on `192.168.1.0/24` using the dashboard | **Accepted.** LAN users are trusted operators with full fleet control and none of Ariel's authorization, until caliban-ai/prospero#2 adds API authentication |
| A tailnet client using the dashboard through the Tailscale subnet router | **Probably closed**: the traffic reaches Traefik from the connector pod's cluster address, which the LAN-only rule rejects. **Verify on the live cluster** with a request from a tailnet device, and restrict the route if it passes |
| A cluster admin using `kubectl port-forward` | **Accepted**: a cluster admin already has full control |
| A compromised Traefik | Out of scope |

### Workload identity to gonzalod

- Ariel authenticates to gonzalod as its own **bearer-token principal** (gonzalo ADR
  0015), scoped to the access-control record namespaces settled in
  caliban-ai/gonzalo#277. This is machine identity, deliberately separate from the
  human identity records it stores.
- **gonzalod auth must be on before Ariel writes identity records.** The gonzalo chart
  gains a principals file mounted from an existing Secret (caliban-ai/helm-charts#48),
  sealed in the private deploy with the `ariel` principal and an admin principal.
  Agents that use gonzalod get tokens in the same rollout, injected by the operator
  (caliban-ai/caliban-operator#41), so switching auth on does not cost them their
  memory. The container docs gain the principals file (caliban-ai/gonzalo#281).
- Ariel's gonzalo client (#15) sends its token from the start. Account linking (#16)
  and two-key authorization (#17) are blocked until gonzalod auth is on.

### Work this creates

| Ticket | Scope |
|---|---|
| caliban-ai/helm-charts#48 | gonzalo chart: principals file from an existing Secret |
| caliban-ai/helm-charts#49 | prospero chart: optional ingress NetworkPolicy with allowed clients |
| caliban-ai/ariel#30 | Image, release workflow, `/healthz`, file-based credentials |
| caliban-ai/helm-charts#50 | Generic `ariel` chart, disabled in the umbrella by default |
| caliban-ai/gonzalo#281 | Document `GONZALO_AUTH_FILE` |
| caliban-ai/caliban-operator#41 | Inject a gonzalod token into agent pods |

The private `johnford2002/helm-charts` rollout, tracked there rather than on the
caliban-ai board:

1. Seal `ariel-credentials` and the gonzalod principals file.
2. Enable prosperod's NetworkPolicy with Traefik and Ariel as clients.
3. Enable gonzalod auth and give agent workspaces their token.
4. Pin and enable the `ariel` chart.
5. Confirm probes, `kubectl port-forward` and the Tailscale path on the live cluster.

## Consequences

- **Positive:** Ariel adds no new platform component and follows every existing
  cluster pattern. It exposes nothing publicly and needs no Kubernetes API access.
  Agents can no longer drive the fleet by calling prosperod directly. Role grants
  cannot be forged by other pods once gonzalod auth is on. Every remaining bypass is
  written down with an owner.
- **Negative:** LAN devices keep full, unaudited fleet control through the dashboard
  until prospero#2 lands. Rollouts cost a few seconds of downtime. Rotating a token
  needs a reseal and a restart. Switching gonzalod auth on spans three repositories
  and blocks Ariel's identity layer until it is done. The Tailscale and probe
  behaviour are reasoned, not yet observed.
- **Revisit if:** caliban-ai/prospero#2 adds API authentication (the dashboard
  exception and possibly the allow-list can then go), the cluster restricts egress, a
  second Ariel replica is needed (Gateway sharding or leader election would then be
  required), or the Tailscale check shows tailnet clients reach the dashboard.
