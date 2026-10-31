
# SAIL — Subsystems & Capabilities

**A local-first, proactive personal intelligence platform.**

SAIL is organized into **20 subsystems**, grouped across **6 functional domains**, built upon a shared Rust-based foundation.

> **Architecture note:** These are logical subsystems and capabilities, not necessarily independently deployed microservices. Implementation is modular, with process isolation used where necessary for security, reliability or performance.

## Architecture Overview

| Domain | Subsystems |
|---|---|
| 🧠 Intelligence & Knowledge | Mind · Arena · Discovery · Lab |
| 🎙️ Interaction & Presence | Dialogue · Vision · Sense · Relay |
| ⚙️ Agency & Creation | Agents · Flow · Canvas · Forge |
| 🧩 Personal & Social | Life · Pulse · Friends · Entertainment |
| 🛡️ Systems & Security | Guardian · Bridge · Sentinel |
| 🖥️ Infrastructure & Compute | Mesh |

---

## 01 — Intelligence & Knowledge

<details open>
<summary><strong>SAIL Mind</strong> — Model orchestration and inference</summary>

- **Model Router** — Selects models based on task requirements.
- **Inference Runtime** — Executes local AI models.
- **Model Residency** — Manages model loading and unloading.
- **Context Manager** — Controls context windows and token budgets.
- **GPU/CPU Backend** — Manages hardware acceleration and inference fallback.

</details>

<details>
<summary><strong>SAIL Arena</strong> — Memory and knowledge</summary>

- **Conversation Memory** — Persistent interaction history.
- **Vector Search** — Semantic retrieval across indexed information.
- **Knowledge Graph** — Structured entities and relationships.
- **Memory Consolidation** — Processes and organizes retained knowledge.
- **Contradiction Audit** — Detects and reviews inconsistent knowledge.

</details>

<details>
<summary><strong>SAIL Discovery</strong> — Proactive intelligence</summary>

- **Proactive Research** — Investigates user-selected interests.
- **Research Echoes** — Explores alternative research perspectives.
- **Foresight** — Evaluates information gaps and research priorities.
- **Opportunity Radar** — Tracks relevant employment, research and learning opportunities.
- **Research Reports** — Produces evidence-grounded findings.

</details>

<details>
<summary><strong>SAIL Lab</strong> — Experimentation and evaluation</summary>

- **Experiment Runner** — Coordinates reproducible experiments.
- **Hypothesis Evaluation** — Examines research hypotheses.
- **Model Benchmarks** — Evaluates model and system performance.
- **Socrates** — Adaptive intellectual questioning and reasoning challenges.
- **Evidence Verification** — Evaluates research claims and supporting evidence.

</details>

## 02 — Interaction & Presence

<details>
<summary><strong>SAIL Dialogue</strong> — Conversational interaction</summary>

- **Speech Recognition**
- **Text-to-Speech**
- **Wake Word**
- **Speaker Verification**
- **Attention Gate**
- **Turn Coordinator**

</details>

<details>
<summary><strong>SAIL Vision</strong> — Visual perception</summary>

- **Camera Capture**
- **Screen Perception**
- **Document Parsing**
- **OCR / OXR**
- **Visual Context**

</details>

<details>
<summary><strong>SAIL Sense</strong> — Environmental awareness</summary>

- **Device Context**
- **Ambient Sensors**
- **Presence**
- **Activity Signals**
- **Shadow Telemetry**

</details>

<details>
<summary><strong>SAIL Relay</strong> — Cross-device communication</summary>

- **Device Registry**
- **Session Handoff**
- **Audio Handoff**
- **State Synchronization**

</details>

## 03 — Agency & Creation

<details>
<summary><strong>SAIL Agents</strong> — Autonomous task execution</summary>

- **Task Planning**
- **Agent Harnesses**
- **Tool Scheduler**
- **Task Validation**
- **Adversary**

</details>

<details>
<summary><strong>SAIL Flow</strong> — Workflow automation</summary>

- **Workflow Engine**
- **Skillsmith**
- **Instinct Triggers**
- **Friction Detector**
- **Automations**

</details>

<details>
<summary><strong>SAIL Canvas</strong> — Dynamic interfaces and artifacts</summary>

- **Dynamic Workspaces**
- **Visual Renderer**
- **Document Studio**
- **Reports & Exports**
- **Artifact Library**

</details>

<details>
<summary><strong>SAIL Forge</strong> — Agentic software development</summary>

- **Coding Agent**
- **Repository Tools**
- **Test Runner**
- **Debugging**
- **Change Proposals**

</details>

## 04 — Personal & Social

<details>
<summary><strong>SAIL Life</strong> — Personal and academic organization</summary>

- **Calendar Context**
- **Course Mirror**
- **Exam Readiness**
- **Group Projects**
- **Possessions**

</details>

<details>
<summary><strong>SAIL Pulse</strong> — Goals and attention management</summary>

- **Reminders**
- **Goals**
- **Deadline Tracking**
- **Study Scheduling**

</details>

<details>
<summary><strong>SAIL Friends</strong> — Social context and memory</summary>

- **Person Profiles**
- **Social Memory**
- **Conversation Context**
- **Relationship Timeline**

</details>

<details>
<summary><strong>SAIL Entertainment</strong> — Media and personal entertainment</summary>

- **Media Library**
- **Audio DNA**
- **Reading Continuity**
- **Release Tracker**
- **Playback & Queues**

</details>

## 05 — Systems & Security

<details>
<summary><strong>SAIL Guardian</strong> — Authorization and protection</summary>

- **Permissions**
- **Policy Enforcement**
- **Credential Vault**
- **Sandbox Authorization**
- **Audit Log**

</details>

<details>
<summary><strong>SAIL Bridge</strong> — Operating system and external integrations</summary>

- **OS Integration**
- **Web Search**
- **Python Analysis**
- **Home Assistant**
- **External Connectors**

</details>

<details>
<summary><strong>SAIL Sentinel</strong> — Security and system reliability</summary>

- **Threat Detection**
- **Service Recovery**
- **Update Auditor**
- **Backup Verification**
- **Network Monitoring**

</details>

## 06 — Infrastructure & Compute

<details>
<summary><strong>SAIL Mesh</strong> — Distributed computing</summary>

- **Worker Discovery**
- **Compute Federation**
- **Resource Placement**
- **Remote Execution**
- **Failover**

</details>

---

## SAIL Core

The shared foundation supporting every subsystem.

| Component | Responsibility |
|---|---|
| **Shared Runtime** | Service lifecycle and execution |
| **Event Bus** | Typed inter-component communication |
| **Durable Job Queue** | Persistent asynchronous task execution |
| **Continuity** | Resumable work across sessions and restarts |
| **Scheduler** | Task prioritization and dispatch |
| **Persistence** | Durable state and local storage |
| **Resource QoS** | Compute, memory and latency prioritization |
| **Health & Observability** | Logs, metrics and service health |

---

## Architectural Principles

1. **Local-first:** Prioritize private, local execution and storage.
2. **Modular:** Subsystems share infrastructure and typed interfaces.
3. **Efficient:** Prefer deterministic computation over unnecessary LLM inference.
4. **Secure:** All consequential actions pass through Guardian.
5. **Persistent:** Background work survives conversation changes and restarts.
6. **Resource-aware:** Schedule demanding tasks without disrupting interactive use.
7. **Interface-conscious:** Add UI only when functionality genuinely benefits from it.
8. **Extensible:** Integrate external tools and services without making them mandatory.

> **Development status:** This document represents SAIL's planned architecture and capabilities. Listing a component does not imply that it has been implemented.
