# SAIL Core Subsystem Contracts & Architecture

This document defines the interface contracts, message schemas, and coordination boundaries among the 8 core subsystems of **SAIL Core**.

## Subsystems Overview

| Subsystem | Primary Responsibility | Input Triggers / Messages | Output Events / Directives |
| :--- | :--- | :--- | :--- |
| **SAIL Dialogue** | Voice & text conversations, turn taking, audio capture | User audio streams, text messages, Agent response streams | User utterance events, interruption signals, transcript entries |
| **SAIL Canvas** | Document, visual diagram, and interactive board rendering | Render requests from Agents & Flow | Visual stream frames, board updates, client render payloads |
| **SAIL Flow** | Automation graphs, trigger listening, scheduled workflows | Discovery triggers, external webhooks, periodic timers | Workflow execution dispatches to Agents, Canvas updates |
| **SAIL Discovery** | Autonomous curiosity, gap identification, proactive research | Arena knowledge graph deltas, user interest updates | Research mission definitions dispatched to Flow |
| **SAIL Agents** | Goal decomposition, tool execution, reasoning loops | Intent from Dialogue, Tasks from Flow | Tool execution requests to Bridge, Memory commits to Arena, Inference to Mind, Visuals to Canvas |
| **SAIL Arena** | Associative memory, knowledge graph, personal context | Memory commit events, graph query requests | Context retrieval bundles, knowledge gap notifications |
| **SAIL Mind** | LLM/SSM inference, model routing, context window control | Inference completion requests from Agents | Streamed token deltas, tool call proposals, embeddings |
| **SAIL Bridge** | Host OS access, sandboxed CLI tools, device connectors | Tool execution requests from Agents | Tool execution results, stderr/stdout, system telemetry |

---

## Inter-Subsystem Interaction Pathways

### 1. User Dialogue & Interaction Loop
```
User <==[Audio / Text]==> SAIL Dialogue <==[Intent / Stream]==> SAIL Agents
```
1. `SAIL Dialogue` ingests raw user speech/text and handles interruption detection (Voice Activity Detection + barge-in).
2. Once speech is transcribed or a text message is received, a `UserIntentEvent` is dispatched to `SAIL Agents`.
3. `SAIL Agents` streams token responses and status back to `SAIL Dialogue`, which forwards text or converts it via TTS to speech for the user.

### 2. Autonomous Proactive Discovery Loop
```
SAIL Arena ==[Knowledge Gaps]==> SAIL Discovery ==[Research Workflow]==> SAIL Flow ==[Tasks]==> SAIL Agents ==[New Knowledge]==> SAIL Arena
```
1. `SAIL Arena` continuously monitors knowledge graph density, tracking topics with high uncertainty or stale facts against user interests.
2. `SAIL Discovery` scores potential research opportunities and emits structured research workflows to `SAIL Flow`.
3. `SAIL Flow` schedules and triggers task steps, handing them to `SAIL Agents`.
4. `SAIL Agents` conducts synthesis, searches sources via `SAIL Bridge`, validates claims, and writes the verified knowledge back into `SAIL Arena`.

### 3. Agent Execution & Tool Loop
```
                 ┌───────────> SAIL Mind (LLM / Embeddings)
                 │
SAIL Agents ────┼───────────> SAIL Bridge (OS / Shell / Tools)
                 │
                 ├───────────> SAIL Arena (Episodic / Semantic Store)
                 │
                 └───────────> SAIL Canvas (Visual Artifacts / UI Cards)
```
- **Mind**: LLM routing (Gemma 4 local GGUF, native SSM/MoE, cloud fallback).
- **Bridge**: Sandboxed execution with permissions, file access, and OS connectors.
- **Arena**: Persistent semantic search and associative link establishment.
- **Canvas**: Render interactive documents, diagrams, and cards directly to client UI.

---

## Core Domain Models & Event Types

### Event Envelope
All subsystem events share a uniform `SailEvent` envelope:

```rust
pub struct SailEvent {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub source: SubsystemId,
    pub correlation_id: Option<Uuid>,
    pub payload: EventPayload,
}
```

### Event Payload Variants
- `DialogueInput`: Utterance text, audio buffer, speaker id.
- `AgentTaskDispatched`: Goal, constraints, parent workflow id.
- `AgentTokenStream`: Incremental token delta, finish reason.
- `ToolExecutionRequest`: Tool name, parameters, permission level.
- `ToolExecutionResult`: Exit code, stdout, stderr, structured data.
- `MemoryCommit`: Vector embedding, graph triplets, episodic summary.
- `CanvasArtifactRender`: Document type (markdown, mermaid, UI card), content, target element.
- `DiscoveryTrigger`: Topic, priority score, hypothesis, search query.
