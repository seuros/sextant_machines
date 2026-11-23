# Sextant Machines

> Take a fresh fix before correcting course. A missing reply is not a fix.

Sextant reconciles one resource against a versioned desired value. It plans one
conditional write at a time; it neither performs writes nor retries them on
timeout. If the reply goes missing, the write may still land, even after the
desired value has changed.

## The Flight Plan

1. **Set the course.** Bind one `Sextant` to one resource and give it an `Intent`.
   Advance the revision with `set_intent` when the desired value changes. A new
   intent doesn't cancel the pending write.
2. **Take a fresh fix.** Call `observe` with a read from the authority that
   enforces the write precondition. Report missing or stale evidence as
   `Observation::Unknown`, not as an absent resource.
3. **Log before dispatch.** On `Apply(action)`, persist the intent, pending action
   and Periscope issuer watermark together. Dispatch the action with an atomic
   compare-and-set against `action.expected`. The action ID correlates receipts;
   the resource version is what fences the write.
4. **If the reply goes missing, read again.** The same token means `Wait`, not
   retry. A changed token proves the old precondition cannot match again, not
   necessarily that Sextant's write succeeded. Alternatively, call `settle` only
   with terminal evidence for that exact action, then read again: a receipt
   cannot prove convergence.

| Observation | Decision |
| --- | --- |
| Missing, stale, or unknown | `ObserveAgain` |
| Still at the pending write's expected version | `Wait`, even if the value already matches the latest intent |
| Fresh authoritative state matches the intent, with no write that can undo it | `Converged` |
| Fresh authoritative state differs and no prior write can still land | `Apply` |

## What's Yours

Serialize observations and decisions for the resource. Its token must change
on every accepted write and never recur, including across deletion, recreation
or restart. An absent resource needs its own token; a bare “not found” is not
enough. Never feed a cached read from before the write was proposed: a different
**older** token would wrongly disarm the action.

Persist the updated state before reporting convergence or settlement, or
sending a replacement. On restart, use `Sextant::restored` with the latest
atomic record and keep the old controller from serving concurrently.

The `no_std` core owns no allocator, clock, storage, network, or fencing
service. Values can be small application types or stable equality keys.

**Status:** Unpublished local core. The Foundation desired-state scenario
simulates a restart and a lost reply; it does not exercise durable storage or
distributed authority.
