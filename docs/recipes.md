# zdk recipes

Short, copy-pasteable patterns. Every invocation resolves against `zdk --help-json`; add `--dry-run` to any write to see the exact request without sending it.

## Triage the unassigned queue with an agent

```bash
zdk tickets list --status new --unassigned --limit 50 -o ndjson \
  | my-triage-agent \
  | jq -r '.[] | "\(.id) \(.assignee)"' \
  | while read -r id who; do zdk tickets assign "$id" --to "$who"; done
```

## Nightly export of everything updated in the last day, resumable

```bash
zdk tickets list --all --updated-after 24h -o ndjson --checkpoint /var/tmp/tickets.ckpt > tickets.ndjson
# interrupted? run the same command again; it resumes from the checkpoint
```

For arbitrary search queries use the export endpoint directly (cursor pagination, no 1,000-result cap):

```bash
zdk search export 'type:ticket created>2026-01-01 tags:vip' --to vip.ndjson
```

## Reply from a file and solve in one go

```bash
zdk tickets solve 1234 --body-file reply.md              # public reply + status solved
zdk tickets note 1234 --body "Escalated to L2."           # internal note (agents only)
```

## Safe update: fail if someone else touched the ticket

```bash
stamp=$(zdk tickets get 1234 | jq -r .updated_at)
zdk tickets update 1234 --status pending --safe-update --updated-stamp "$stamp"   # 409 → VALIDATION, exit 6
```

## Find a user by email, then their open tickets

```bash
uid=$(zdk users get jane@acme.com | jq .id)
zdk tickets list --requester "$uid" --status open,pending
```

## Rate-limit-aware bulk tagging

```bash
zdk tickets list --tag old-name --all -o ndjson | jq -r .id | while read -r id; do
  zdk tickets update "$id" --add-tag new-name --remove-tag old-name
done
# the governor keeps this under 100 ticket updates/min and 30 per ticket per 10 min;
# add --rate-limit-strategy fail to exit 7 instead of waiting
```

## Endpoints without a curated command

```bash
zdk api ops --grep macros --method GET                      # what exists
zdk api describe ListMacros                                 # parameters, pagination dialect, scope
zdk api GET /api/v2/macros --paginate -o ndjson | jq -r '.title'
zdk api GET /api/v2/tickets/1234/macros/98765/apply --dry-run   # "apply macro" is a GET that returns the resulting ticket
```

## Generate tool definitions for an LLM

```bash
zdk --help-json | jq '[.subcommands[] | select(.name == "tickets") | .subcommands[] | {name: (.path | join(" ")), description: .summary, flags: [.flags[].name]}]'
```

## CI job with a confidential client

```bash
export ZENDESK_SUBDOMAIN=acme ZENDESK_CLIENT_ID=zdk_ci ZENDESK_CLIENT_SECRET="$SECRET"
export ZENDESK_CREDENTIAL_STORE=file ZENDESK_CREDENTIALS_PASSPHRASE="$PASSPHRASE"
zdk auth login --client-credentials --scopes tickets:read,users:read
zdk tickets count --status open -o json
```

## Check what a token can do before running a workflow

```bash
zdk auth scopes check tickets update && zdk auth scopes check users create
zdk doctor --json | jq '.checks[] | select(.status != "ok")'
```
