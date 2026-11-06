# Arena Learn — recorded real local-model journey

Recorded: 2026-11-06T13:35:27.118181536+00:00
Model: `Qwen3.6-35B-A3B-Q4_K_M.gguf` via `http://127.0.0.1:8081/v1/chat/completions`
Limits: inference timeout 1800000 ms, max output 1024 tokens

This is one recorded run on a CPU-only host with a real local llama.cpp model through
the Arena Learn engine. It is acceptance evidence, not a benchmark; model quality and
timings vary. The raw JSON beside this file contains the full transcripts.

## What the run demonstrates

- Session 1 answered 6 questions and recorded 5 incorrect/failed-judgment answers (an observed misconception).
- Session 1 plan: present; revision 1; 5 nodes.
- Session 1 probe summary: present.
- Session 2 received 3 extracted graph propositions as explicit context.
- Graph: 5 propositions, 4 chains.

## Session 1 — Understand TCP congestion control

Session: `learn_728e2f9a65e343e782f0e391043e1656`; mode `teach`; tokens 40291 prompt / 1902 completion.

### Probe summary

```json
{
  "summary": "The learner has limited understanding of TCP congestion control. They do not seem to know that routers drop packets when buffers are full, that missing ACKs indicate packet loss, or that congestion control aims to prevent network overload. They also do not recognize the sliding window protocol as a key mechanism.",
  "known": [],
  "gaps": [
    "Understanding that routers drop packets during congestion",
    "Recognizing that missing ACKs signal packet loss",
    "Knowing the purpose of TCP congestion control (preventing overload)",
    "Identifying the sliding window protocol as a congestion control mechanism"
  ]
}
```

### Questions and observed answers

- [incorrect] When sending data over the internet, what happens if too many packets arrive at a router at once and its buffer is full?
  - answer: `{"choices": ["a"], "confidence_pct": 65}`
  - correct: `['b']`
- [incorrect] If a sender receives no acknowledgment (ACK) for a sent packet, what is the most likely reason in TCP?
  - answer: `{"choices": ["d"], "confidence_pct": 65}`
  - correct: `['a']`
- [incorrect] What is the main purpose of TCP congestion control?
  - answer: `{"choices": ["d"], "confidence_pct": 65}`
  - correct: `['b']`
- [incorrect] Which of these is a common mechanism used by TCP to handle congestion?
  - answer: `{"choices": ["d"], "confidence_pct": 65}`
  - correct: `['a']`
- [correct] If a router's buffer is full, what happens to new incoming packets?
  - answer: `{"choices": ["a"], "confidence_pct": 65}`
  - correct: `['a']`
- [incorrect] If a TCP sender does not receive an ACK for a sent packet, what does it most likely assume?
  - answer: `{"choices": ["d"], "confidence_pct": 65}`
  - correct: `['a']`
- [unanswered] What does TCP do when it detects a missing ACK?
  - answer: `null`
  - correct: `None`

### Teacher plan

```json
{
  "revision": 1,
  "goal": "goal",
  "nodes": [
    {
      "id": "routers_drop",
      "title": "Routers drop packets during congestion"
    },
    {
      "id": "missing_acks",
      "title": "Missing ACKs signal packet loss"
    },
    {
      "id": "congestion_purpose",
      "title": "Purpose of TCP congestion control"
    },
    {
      "id": "sliding_window",
      "title": "Sliding window protocol"
    },
    {
      "id": "goal",
      "title": "Understand TCP congestion control"
    }
  ],
  "edges": [
    [
      "routers_drop",
      "missing_acks"
    ],
    [
      "missing_acks",
      "congestion_purpose"
    ],
    [
      "congestion_purpose",
      "sliding_window"
    ],
    [
      "sliding_window",
      "goal"
    ]
  ],
  "order": [
    "routers_drop",
    "missing_acks",
    "congestion_purpose",
    "sliding_window",
    "goal"
  ],
  "mermaid": "flowchart TD\n  routers_drop[\"Routers drop packets during congestion\"]\n  missing_acks[\"Missing ACKs signal packet loss\"]\n  congestion_purpose[\"Purpose of TCP congestion control\"]\n  sliding_window[\"Sliding window protocol\"]\n  goal[\"Understand TCP congestion control\"]\n  routers_drop --> missing_acks\n  missing_acks --> congestion_purpose\n  congestion_purpose --> sliding_window\n  sliding_window --> goal\n"
}
```

### Repair after a failed answer

The first teaching quiz was answered incorrectly; the teacher then explained
the same node again:

> When an ACK is missing, TCP assumes the packet was lost—usually because a router dropped it during congestion. This triggers retransmission and adjustments to avoid further overload.


## Session 2 — Graph reuse

Session: `learn_13d63bd1c188463483c46dabfefca9b0`; mode `teach`; tokens 21275 prompt / 1300 completion.

### Context passed from the first session

```json
[
  {
    "name": "missing acks signal packet loss",
    "statement": "When an ACK is missing, TCP assumes the packet was lost\u2014usually because a router dropped it during congestion. This triggers retransmission and adjustments to avoid further overload."
  },
  {
    "name": "purpose of tcp congestion control",
    "statement": "Purpose of TCP congestion control (part of Understand TCP congestion control)"
  },
  {
    "name": "routers drop packets during congestion",
    "statement": "When too many packets arrive at a router at once, its buffer fills up. Once full, the router must drop incoming packets to keep processing the ones it has."
  }
]
```

### Second session plan

```json
{
  "revision": 1,
  "goal": "goal",
  "nodes": [
    {
      "id": "step1",
      "title": "Reduced Transmission Rate"
    },
    {
      "id": "step2",
      "title": "Delayed Resource Arrival"
    },
    {
      "id": "step3",
      "title": "Multi-Resource Dependencies"
    },
    {
      "id": "goal",
      "title": "Overall Web Page Latency"
    }
  ],
  "edges": [
    [
      "step1",
      "step2"
    ],
    [
      "step2",
      "step3"
    ],
    [
      "step3",
      "goal"
    ]
  ],
  "order": [
    "step1",
    "step2",
    "step3",
    "goal"
  ],
  "mermaid": "flowchart TD\n  step1[\"Reduced Transmission Rate\"]\n  step2[\"Delayed Resource Arrival\"]\n  step3[\"Multi-Resource Dependencies\"]\n  goal[\"Overall Web Page Latency\"]\n  step1 --> step2\n  step2 --> step3\n  step3 --> goal\n"
}
```

## Extracted graph

```json
[
  {
    "id": "prop-learn_728e2f9a65e343e782f0e391043e1656-missing_acks",
    "name": "missing acks signal packet loss",
    "statement": "When an ACK is missing, TCP assumes the packet was lost\u2014usually because a router dropped it during congestion. This triggers retransmission and adjustments to avoid further overload.",
    "aliases": [
      "missing_acks"
    ],
    "created_at": 1793971713499,
    "provenance": [
      {
        "kind": "teaching",
        "session_id": "learn_728e2f9a65e343e782f0e391043e1656",
        "label": "Understand TCP congestion control",
        "created_at": 1793971713499
      }
    ]
  },
  {
    "id": "prop-learn_728e2f9a65e343e782f0e391043e1656-congestion_purpose",
    "name": "purpose of tcp congestion control",
    "statement": "Purpose of TCP congestion control (part of Understand TCP congestion control)",
    "aliases": [
      "congestion_purpose"
    ],
    "created_at": 1793971713499,
    "provenance": [
      {
        "kind": "teaching",
        "session_id": "learn_728e2f9a65e343e782f0e391043e1656",
        "label": "Understand TCP congestion control",
        "created_at": 1793971713499
      }
    ]
  },
  {
    "id": "prop-learn_728e2f9a65e343e782f0e391043e1656-routers_drop",
    "name": "routers drop packets during congestion",
    "statement": "When too many packets arrive at a router at once, its buffer fills up. Once full, the router must drop incoming packets to keep processing the ones it has.",
    "aliases": [
      "routers_drop"
    ],
    "created_at": 1793971713499,
    "provenance": [
      {
        "kind": "teaching",
        "session_id": "learn_728e2f9a65e343e782f0e391043e1656",
        "label": "Understand TCP congestion control",
        "created_at": 1793971713499
      }
    ]
  },
  {
    "id": "prop-learn_728e2f9a65e343e782f0e391043e1656-sliding_window",
    "name": "sliding window protocol",
    "statement": "Sliding window protocol (part of Understand TCP congestion control)",
    "aliases": [
      "sliding_window"
    ],
    "created_at": 1793971713499,
    "provenance": [
      {
        "kind": "teaching",
        "session_id": "learn_728e2f9a65e343e782f0e391043e1656",
        "label": "Understand TCP congestion control",
        "created_at": 1793971713499
      }
    ]
  },
  {
    "id": "prop-learn_728e2f9a65e343e782f0e391043e1656-goal",
    "name": "understand tcp congestion control",
    "statement": "Understand TCP congestion control (part of Understand TCP congestion control)",
    "aliases": [
      "goal"
    ],
    "created_at": 1793971713499,
    "provenance": [
      {
        "kind": "teaching",
        "session_id": "learn_728e2f9a65e343e782f0e391043e1656",
        "label": "Understand TCP congestion control",
        "created_at": 1793971713499
      }
    ]
  }
]
```

## Reproduce

```sh
SAIL_LLM_URL=http://127.0.0.1:8081/v1/chat/completions \
SAIL_MODEL_PATH=models/Qwen3.6-35B-A3B-Q4_K_M.gguf \
SAIL_JOURNEY_OUT=/tmp/arena-learn-journey.json \
  cargo test -p sail-dialogue --test real_learn_journey -- --ignored --nocapture
```

## Honest limitations

- CPU-only host: the AMD Vulkan device was unavailable, so the run used llama.cpp on CPU.
- One run, one model, one machine; timings and answer quality are not benchmarks.
- The learner answers are a deterministic acceptance policy, not a human study.
- The browser UI, Android/WebView and screen-reader paths were not exercised here
  (see the browser tests for those surfaces).
- The model's own prose may include mistakes; the record shows what SAIL stored, not a
  claim that every explanation is factually perfect.
