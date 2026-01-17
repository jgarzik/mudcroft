# IgorMUD and Programmable MUD Engines — AI Coding Agent Briefing

## Purpose
This briefing gives an AI coding agent enough context to:
- Understand what **IgorMUD** likely was architecturally (an **LP-MUD-family** game)
- Understand the **programmable MUD engine** theory of the era (late 80s–90s)
- Understand the **runtime C-derivative language** used by LP-MUDs (**LPC**) and how it shaped object/world design
- Compare with other programmable MUD families (MOO, MUCK, MUSH, Diku descendants)
- Provide a mental model for implementing or emulating a “programmable MUD” engine today

> Working assumption: IgorMUD was an LP-style MUD. The language you remember as a “runtime C derivative” aligns strongly with **LPC (Lars Pensjö C)**.

---

## 1) Big Picture: “Programmable MUD” Engine Model
Programmable MUDs differ from “hardcoded” MUDs because **the world logic is code**, not just data.

### Core architectural split (LP-MUD model)
- **Driver (native binary, historically written in C):**
  - Network sockets
  - VM/runtime
  - Scheduler
  - Memory management + garbage collection
  - Sandboxing
  - Built-in primitives (aka “efuns”)
- **Mudlib (world code, written in LPC):**
  - Rooms, items, NPCs, players
  - Combat, spells, skills
  - Command parsing and dispatch
  - Economy, quests, guilds
  - Security policy

Key idea: **the “game engine” lives in the mudlib**, and the driver is a general-purpose, secure execution environment.

### Why this design felt magical
- You can add/modify world behavior by editing code files, recompiling objects, and (depending on driver) even upgrading parts of the world **without rebooting**.
- Builders/wizards could implement *new mechanics* (items, spells, guilds, areas) in the same language used by the core engine.

---

## 2) The Language: LPC (the “runtime C derivative”)
### What LPC is
- A **C-like** language designed for LP-MUDs.
- Aimed at fast iteration and safe sandboxing.
- Includes strong world/runtime integration through efuns and driver hooks.

### The mental model: “Objects are code files”
- In typical LP systems, each object is defined by a **source file**.
- Loading a file compiles it into a **blueprint/prototype** object.
- Instances are usually created as **clones** of that blueprint.

The code file typically defines:
- State (member variables)
- Methods (functions)
- Inheritance (reuse and composability)
- Hooks for events like movement, interaction, combat ticks

### Inheritance-heavy composition
Common patterns:
- `room` inherits `/std/room`
- `weapon` inherits `/std/weapon`
- `npc` inherits `/std/monster`

This creates an ecosystem of reusable standard library objects.

### Efuns, simul_efuns, and privileged control
- **Efuns:** primitives provided by the driver (move objects, messaging, filesystem access, scheduling, security checks, etc.).
- **Simul_efuns:** mudlib-provided “global functions” typically loaded automatically to feel like a standard library.
- **Master object:** a special mudlib object that mediates security/policy decisions between driver and mudlib.

The combination is how LP systems keep programmability while avoiding letting every wizard trivially break security.

---

## 3) World Modeling: Object Graph + Containment
LP worlds model the game as a graph of objects with containment:
- **Rooms** contain objects (players, NPCs, items)
- Players/NPCs contain inventories
- Every object has an **environment** (where it is)

### Movement
Movement is typically:
- `move_object(obj, dest)` (or an equivalent mudlib function)
Then the engine triggers hooks so objects can react.

### Living objects
Players and NPCs are typically “living” objects:
- “Living” implies:
  - command input handling
  - combat participation
  - stats (hp, attributes)
  - sometimes heartbeat/tick updates

---

## 4) Event Model: init(), heartbeats, and timers
Programmable MUDs rely on consistent event hooks.

### `init()`
Typical conceptual behavior:
- When object A enters a new environment (room or container), objects in that environment get a chance to:
  - add commands
  - detect presence
  - initialize interaction rules

This is why “verbs” can be contextual (holding an item adds commands; being in a room adds exits; etc.).

### `heart_beat()` (tick-based updates)
Used for:
- NPC AI
- regeneration
- ongoing combat
- status effects

### `call_out()` (delayed callbacks)
Used for:
- spells and cooldowns
- delayed actions
- timed combat swings
- room resets

---

## 5) Commands and Verbs: How Player Input Maps to Code
Common mudlib approaches:

### A) Contextual action tables
- Objects add “actions” to the player when relevant (often via `init()`).
- Example conceptual flow:
  - you enter a room
  - room adds exit commands
  - items in the room add “get”, “look”, “read” variants
  - your inventory adds “wear”, “wield” commands

### B) Central command daemons
- Input is parsed centrally
- Commands map to command objects or functions
- Context is checked by querying the world graph

Both models can coexist.

---

## 6) Combat Theory: PvM and PvP as Policy on the Same Plumbing
Most LP-style systems implement combat on “living” objects, so PvM and PvP share the pipeline.

### Combat pipeline (conceptual)
1. An attack action establishes attacker/target relationships.
2. Combat rounds advance via:
   - a combat daemon tick, or
   - each living’s heartbeat, or
   - scheduled callouts.
3. Resolution calls overridable hooks:
   - hit resolution
   - damage application
   - death handling

NPC AI (aggro, assist, flee) is typically heartbeat/callout-driven.

### PvP differences are mostly rules/policy
PvP often differs by:
- Is attacking players allowed?
- Only in arenas or flagged zones?
- Consent/duel flags?
- level-range limitations?
- penalties (bounty, jail, corpse loss, reputation)?

If IgorMUD was “non-PK,” then player-targeted attacks are likely rejected globally or only allowed in controlled contexts (e.g., arena).

---

## 7) Persistence and Hot Upgrade Philosophy (DGD angle)
Some LP descendants (notably DGD) emphasized:
- persistence across reboots
- upgrading objects without full shutdown

This shapes world evolution:
- objects are long-lived
- upgrades must handle schema/state migration
- the engine needs versioned interfaces or conversion steps

---

## 8) Similar Programmable MUD Families (Era Taxonomy)
These are “programmable” but with different programming models.

### LP-MUDs (IgorMUD-style)
- **Language:** LPC
- **Model:** driver + mudlib; objects as code files; inheritance-heavy
- **Strength:** deeply code-defined world mechanics

### MOO (e.g., LambdaMOO)
- **Language:** MOO’s own object/verb language
- **Model:** objects have properties + verbs; strong in-world permissions
- **Strength:** social worlds + user-created interactive behaviors

### MUCK (TinyMUCK / Fuzzball)
- **Language:** MUF (Forth-like)
- **Model:** programmable commands and objects with stack-based scripts
- **Strength:** user scripting, fast customization, roleplay tools

### MUSH/MUX
- **Language:** “softcode” macro/scripting systems
- **Model:** builders script behaviors via embedded language
- **Strength:** easy-to-use, less “systems programming” heavy

### DIKU/ROM/SMAUG descendants
- **Language:** mostly C engine; later scripting layers (mobprogs/DG scripts)
- **Strength:** fast combat-oriented gameplay; less “everything is code objects”

---

## 9) Implementation Notes (If You’re Building a Modern Programmable MUD)
If the goal is to emulate LP-MUD programmability today, key components include:

### VM/runtime layer
- A sandboxed language runtime (you can implement LPC-like semantics or host another language)
- Object lifecycle management
- Scheduler (tick + timers)
- Persistence hooks (optional)

### World object system
- Prototype/clone model
- Containment graph (environment/inventory)
- Standard library object hierarchy

### Event hooks
- movement triggers
- init-style cross-object handshake
- periodic ticks (heartbeat)
- delayed callbacks (callout)

### Security model
- capability-based or role-based permissions
- clear separation of:
  - trusted core engine code
  - builder code
  - player-driven scripts

### Combat engine
- unified pipeline for PvM and PvP
- policy layer controlling whether/where PvP is legal

---

## 10) Questions to Resolve for Historical Accuracy (Optional)
If the user can provide any of the following, the agent can align to the exact historical stack:
- Approximate years played (driver generation + mudlib era)
- Any remembered commands (e.g., `ed`, `update`, `clone`, `inherit`, wizard tools)
- Whether it was MudOS, LPMud 2.4.5, DGD, or another driver
- Whether PvP existed at all, or only arena-style

---

## Summary
IgorMUD-style programmability is best explained by the LP-MUD concept:
- A small secure **driver** hosts a C-like sandboxed language (**LPC**)
- A large **mudlib** implements the world as composable code objects
- The game loop emerges from event hooks (`init`, heartbeat, timers)
- PvM/PvP share a combat pipeline; PvP is a policy layer

This is a powerful, extensible engine model that can be recreated today with a modern sandboxed runtime and an object-graph world model.

