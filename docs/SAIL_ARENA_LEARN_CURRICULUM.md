# SAIL Arena Learn — Curriculum Specification

**Status:** Authoritative clean-room curriculum outline for Arena Learn interactive courses.  
**Tracks:** Rust & Systems Programming (`ready`), Philosophy & Logic (`planned`), Physics, Math & Engineering (`planned`).  
**Tracking Issue:** [#386](https://github.com/PedroVictorCoding/SAIL/issues/386).  
**Pedagogical & Architectural Principles:** See [SAIL_ARENA_LEARN.md](SAIL_ARENA_LEARN.md) and [LLM_Handoff.md](/home/main/sail-worktrees/LLM_Handoff.md).

---

## 1. Pedagogical Architecture & Invariants

1. **Clean-Room Guarantee:** All lessons, learning objectives, diagnostics, and exercises are original SAIL materials. No content, exercises, or prompts are copied or paraphrased from external platforms (e.g., Duolingo, Brilliant, Rustlings, Exercism).
2. **Pedagogical Modality:** No artificial gamification (no streaks, gems, XP, leagues, or mascots). The user interface emphasizes deliberate cognitive practice, precise mental models, and honest retention tracking.
3. **Evidence Projection Model:**
   - Single immediate passes are certified as `provisional`.
   - Assisted attempts (using hint levels 1–4 or solution reveal) are certified as `assisted` or `exposure`.
   - `retained` status is granted only after an unassisted pass on an *unseen review variant* across spaced intervals (1, 3, 7, 14, 30 days).
4. **Deterministic Evaluation:**
   - Code exercises (`rust_edit`) execute exclusively inside the isolated `sail-runner` bubblewrap sandbox. If the sandbox is unreachable, the result is reported as `"unavailable"`, never `"incorrect"`.
   - Logic exercises evaluate deductive validity purely by formal structure via `crates/sail-arena-learn/src/courses/logic.rs`. No philosophical propositions are ever evaluated as ontologically true or false.
   - Text, choice, prediction, and ordering exercises are evaluated deterministically with zero runtime LLM calls.
5. **Step Kinds:**
   - `explain`: Concept exposition without submission requirement.
   - `predict`: Predict execution output, compilation error, or state transition.
   - `choice`: Single or multi-selection conceptual evaluation.
   - `order`: Sequencing operations, execution order, or derivation steps.
   - `fill`: Cloze expression completion (e.g. lifetime annotations, borrow specifiers).
   - `rust_edit`: Interactive coding step executed via `sail-runner`.
   - `logic_check`: Deductive argument validity and counterexample derivation.

---

## 2. Track 1: Rust & Systems Programming (`ready`)

### Chapter 1: Memory, Affine Types & Move Semantics
Focus: Stack vs. heap layout, linear/affine resource management, destruction order, move semantics.

#### Lesson 1.1: Stack Frames and Value Representation
- **Objective ID:** `rust.mem.stack_layout`
- **Estimated Duration:** 5 minutes
- **Prerequisites:** None
- **Exercise Kinds:** `explain`, `predict`, `choice`
- **Core Mental Model:** Fixed-size variables reside directly in the activation frame; pointer-sized indirection addresses dynamically sized buffers.
- **Unseen Review Variants:** 3 variants testing size, alignment, and frame teardown order.

#### Lesson 1.2: Ownership Transfer & Affine Invalidation
- **Objective ID:** `rust.own.move_semantics`
- **Estimated Duration:** 6 minutes
- **Prerequisites:** `rust.mem.stack_layout`
- **Exercise Kinds:** `predict`, `choice`, `order`
- **Core Mental Model:** Binding reassignment transfers ownership; previous binding becomes inaccessible at compile-time with zero runtime overhead.
- **Unseen Review Variants:** 4 variants testing moves into function calls, struct fields, and conditional branches.

#### Lesson 1.3: The `Copy` Trait vs. Clone
- **Objective ID:** `rust.own.copy_vs_clone`
- **Estimated Duration:** 5 minutes
- **Prerequisites:** `rust.own.move_semantics`
- **Exercise Kinds:** `choice`, `predict`, `rust_edit`
- **Core Mental Model:** Bitwise copy semantics (`Copy`) vs. explicit allocation duplication (`Clone`). Types managing non-trivial heap resources cannot implement `Copy`.
- **Unseen Review Variants:** 3 variants predicting compile outcomes when passing types with and without `Copy`.

---

### Chapter 2: The Borrow Checker & Exclusive Access
Focus: Shared vs. exclusive references, non-lexical lifetimes, aliasing XOR mutability theorem.

#### Lesson 2.1: Shared References and Read Immutability
- **Objective ID:** `rust.borrow.shared`
- **Estimated Duration:** 6 minutes
- **Prerequisites:** `rust.own.move_semantics`
- **Exercise Kinds:** `explain`, `predict`, `choice`
- **Core Mental Model:** Multiple concurrent read references (`&T`) permit shared observation while forbidding mutation and invalidation of the referent.
- **Unseen Review Variants:** 3 variants testing simultaneous reads and out-of-scope drops.

#### Lesson 2.2: Exclusive Mutation and Aliasing Invariants
- **Objective ID:** `rust.borrow.aliasing`
- **Estimated Duration:** 7 minutes
- **Prerequisites:** `rust.borrow.shared`
- **Exercise Kinds:** `predict`, `order`, `rust_edit`
- **Core Mental Model:** At any point in time, a memory location may have *either* any number of immutable references (`&T`) *or* exactly one mutable reference (`&mut T`), never both.
- **Unseen Review Variants:** 4 variants identifying conflicting borrows across loops and match arms.

#### Lesson 2.3: Non-Lexical Lifetimes (NLL) and Last-Use Dropping
- **Objective ID:** `rust.borrow.nll`
- **Estimated Duration:** 6 minutes
- **Prerequisites:** `rust.borrow.aliasing`
- **Exercise Kinds:** `predict`, `fill`, `choice`
- **Core Mental Model:** Lifetimes end at the last point of use rather than the syntactic close-brace of the containing block scope.
- **Unseen Review Variants:** 3 variants predicting whether an overlapping borrow compiles under 2018/2021 NLL rules.

---

### Chapter 3: Explicit Lifetimes & Parameter Invariants
Focus: Lifetime bounds on structs and functions, subtyping, variance, and elision rules.

#### Lesson 3.1: Lifetime Elision Rules
- **Objective ID:** `rust.lifetime.elision`
- **Estimated Duration:** 5 minutes
- **Prerequisites:** `rust.borrow.nll`
- **Exercise Kinds:** `explain`, `fill`, `choice`
- **Core Mental Model:** The 3 compiler elision rules: assigning distinct lifetimes to each input parameter, propagating single input lifetime to output, and propagating `&self`/`&mut self` to output.
- **Unseen Review Variants:** 3 variants filling in the desugared explicit lifetime syntax for elided signatures.

#### Lesson 3.2: Structs Storing References
- **Objective ID:** `rust.lifetime.struct_bounds`
- **Estimated Duration:** 7 minutes
- **Prerequisites:** `rust.lifetime.elision`
- **Exercise Kinds:** `predict`, `fill`, `rust_edit`
- **Core Mental Model:** Structs containing references cannot outlive the underlying referents (`'a` parameter bounds).
- **Unseen Review Variants:** 4 variants fixing compilation errors in data structs referencing borrowed slices.

---

### Chapter 4: Smart Pointers, Interior Mutability & Concurrency
Focus: `Box`, `Rc`, `Arc`, `Cell`, `RefCell`, `Mutex`, `RwLock`, `Send` and `Sync`.

#### Lesson 4.1: Heap Indirection and Recursive Data Types
- **Objective ID:** `rust.ptr.box`
- **Estimated Duration:** 5 minutes
- **Prerequisites:** `rust.mem.stack_layout`
- **Exercise Kinds:** `choice`, `predict`, `rust_edit`
- **Core Mental Model:** Sizing dynamically unbounded recursive types via `Box<T>`.
- **Unseen Review Variants:** 3 variants constructing recursive structures (linked lists, expression trees).

#### Lesson 4.2: Shared Ownership: `Rc` vs. `Arc`
- **Objective ID:** `rust.ptr.rc_arc`
- **Estimated Duration:** 6 minutes
- **Prerequisites:** `rust.ptr.box`
- **Exercise Kinds:** `predict`, `choice`, `order`
- **Core Mental Model:** Reference counting with non-atomic vs. atomic fetch-and-add operations; single-thread restriction of `Rc` via negative `Send`/`Sync` markers.
- **Unseen Review Variants:** 3 variants diagnosing thread-boundary compile errors.

#### Lesson 4.3: Interior Mutability: `Cell` and `RefCell`
- **Objective ID:** `rust.mut.interior`
- **Estimated Duration:** 8 minutes
- **Prerequisites:** `rust.borrow.aliasing`, `rust.ptr.rc_arc`
- **Exercise Kinds:** `predict`, `choice`, `rust_edit`
- **Core Mental Model:** Dynamic runtime borrow checking with panic on collision (`RefCell`) vs. copy-based value replacement (`Cell`).
- **Unseen Review Variants:** 4 variants predicting runtime `BorrowMutError` panics versus compile-time guarantees.

#### Lesson 4.4: Concurrency Invariants: `Send`, `Sync`, and Data-Race Freedom
- **Objective ID:** `rust.concurrency.send_sync`
- **Estimated Duration:** 8 minutes
- **Prerequisites:** `rust.mut.interior`
- **Exercise Kinds:** `choice`, `predict`, `fill`
- **Core Mental Model:** `Send` indicates ownership transfer across threads is safe; `Sync` indicates shared reference `&T` across threads is safe. Relationship: `T: Sync <=> &T: Send`.
- **Unseen Review Variants:** 4 variants classifying standard library types (`Rc`, `Arc`, `MutexGuard`, `RefCell`) by their `Send`/`Sync` capabilities.

---

## 3. Track 2: Philosophy & Logic (`planned`)

*Status: Planned. Evaluated using formal propositional validity checkers; grades form and deductive validity without passing ontological judgment on empirical or philosophical claims.*

### Chapter 1: Foundations of Propositional Logic
Focus: Atomic propositions, truth functionality, logical connectives, and syntax trees.

#### Lesson 1.1: Atomic Propositions and Truth Functions
- **Objective ID:** `logic.prop.atoms`
- **Estimated Duration:** 4 minutes
- **Prerequisites:** None
- **Exercise Kinds:** `explain`, `choice`, `order`
- **Core Mental Model:** Propositions as truth-evaluable declarative claims. Truth functionality: truth value of a compound expression is solely determined by the truth values of its constituent parts.
- **Unseen Review Variants:** 3 variants distinguishing declarative propositions from imperative/interrogative sentences.

#### Lesson 1.2: Connectives: Negation, Conjunction, and Disjunction
- **Objective ID:** `logic.prop.connectives`
- **Estimated Duration:** 6 minutes
- **Prerequisites:** `logic.prop.atoms`
- **Exercise Kinds:** `choice`, `fill`, `logic_check`
- **Core Mental Model:** Formalization of `¬` (not), `∧` (and), `∨` (inclusive or). Constructing truth tables with $2^n$ rows.
- **Unseen Review Variants:** 4 variants generating truth table evaluations for multi-connective compound formulas.

#### Lesson 1.3: Material Implication and the Conditional
- **Objective ID:** `logic.prop.conditional`
- **Estimated Duration:** 7 minutes
- **Prerequisites:** `logic.prop.connectives`
- **Exercise Kinds:** `predict`, `choice`, `logic_check`
- **Core Mental Model:** The material conditional `P → Q` is false if and only if `P` is true and `Q` is false. Vacuous truth when antecedent is false. Distinguishing material implication from causal or temporal connection.
- **Unseen Review Variants:** 4 variants evaluating truth conditions of conditionals with false antecedents.

#### Lesson 1.4: Material Equivalence (The Biconditional)
- **Objective ID:** `logic.prop.biconditional`
- **Estimated Duration:** 5 minutes
- **Prerequisites:** `logic.prop.conditional`
- **Exercise Kinds:** `choice`, `fill`, `logic_check`
- **Core Mental Model:** `P ↔ Q` as `(P → Q) ∧ (Q → P)`. Necessary and sufficient conditions.
- **Unseen Review Variants:** 3 variants testing equivalence between statements in natural language and formal notation.

---

### Chapter 2: Deductive Validity, Soundness & Inference Rules
Focus: Deductive validity vs. empirical soundness, inference rules, formal derivations.

#### Lesson 2.1: Validity vs. Soundness
- **Objective ID:** `logic.deduct.validity_soundness`
- **Estimated Duration:** 6 minutes
- **Prerequisites:** `logic.prop.conditional`
- **Exercise Kinds:** `explain`, `choice`, `predict`
- **Core Mental Model:** An argument is valid if and only if it is impossible for all premises to be true and the conclusion false simultaneously. An argument is sound if and only if it is valid and all premises are factually true.
- **Unseen Review Variants:** 3 variants evaluating validity of arguments with false premises.

#### Lesson 2.2: Modus Ponens and Modus Tollens
- **Objective ID:** `logic.deduct.modus_ponens_tollens`
- **Estimated Duration:** 6 minutes
- **Prerequisites:** `logic.deduct.validity_soundness`
- **Exercise Kinds:** `predict`, `choice`, `logic_check`
- **Core Mental Model:** Valid argument forms:
  - Modus Ponens: $P \to Q, P \vdash Q$
  - Modus Tollens: $P \to Q, \neg Q \vdash \neg P$
- **Unseen Review Variants:** 4 variants distinguishing valid applications from surface-similar informal claims.

#### Lesson 2.3: Disjunctive and Hypothetical Syllogisms
- **Objective ID:** `logic.deduct.syllogisms`
- **Estimated Duration:** 6 minutes
- **Prerequisites:** `logic.deduct.modus_ponens_tollens`
- **Exercise Kinds:** `order`, `fill`, `logic_check`
- **Core Mental Model:**
  - Disjunctive Syllogism: $P \lor Q, \neg P \vdash Q$
  - Hypothetical Syllogism (transitivity): $P \to Q, Q \to R \vdash P \to R$
- **Unseen Review Variants:** 3 variants ordering multi-step chained syllogistic deductions.

---

### Chapter 3: Formal Fallacies & Counterexample Construction
Focus: Structural errors in deductive form, counterexample truth assignments.

#### Lesson 3.1: Fallacy of Affirming the Consequent
- **Objective ID:** `logic.fallacy.affirming_consequent`
- **Estimated Duration:** 5 minutes
- **Prerequisites:** `logic.deduct.modus_ponens_tollens`
- **Exercise Kinds:** `predict`, `choice`, `logic_check`
- **Core Mental Model:** $P \to Q, Q \vdash P$ is invalid. Counterexample assignment: $P = \text{false}, Q = \text{true}$ renders all premises true while conclusion is false.
- **Unseen Review Variants:** 3 variants constructing exact truth-value counterexamples.

#### Lesson 3.2: Fallacy of Denying the Antecedent
- **Objective ID:** `logic.fallacy.denying_antecedent`
- **Estimated Duration:** 5 minutes
- **Prerequisites:** `logic.deduct.modus_ponens_tollens`
- **Exercise Kinds:** `predict`, `choice`, `logic_check`
- **Core Mental Model:** $P \to Q, \neg P \vdash \neg Q$ is invalid. Counterexample assignment: $P = \text{false}, Q = \text{true}$ leaves antecedent false (premise true) and conclusion false.
- **Unseen Review Variants:** 3 variants identifying the formal error in everyday reasoning examples.

#### Lesson 3.3: Undistributed Middle and Scope Ambiguities
- **Objective ID:** `logic.fallacy.undistributed_middle`
- **Estimated Duration:** 6 minutes
- **Prerequisites:** `logic.fallacy.affirming_consequent`
- **Exercise Kinds:** `choice`, `predict`, `logic_check`
- **Core Mental Model:** The fallacy of chaining two conditionals that share a consequent ($P \to Q, R \to Q \vdash P \to R$) rather than an antecedent-consequent link.
- **Unseen Review Variants:** 3 variants deriving counterexamples using automated truth tables.

---

### Chapter 4: Epistemic Warrant & Rational Justification
Focus: Theories of justification, foundationalism, coherentism, reliabilism, Gettier problems.

#### Lesson 4.1: The Tripartite Account of Knowledge
- **Objective ID:** `phil.epistem.jtb`
- **Estimated Duration:** 5 minutes
- **Prerequisites:** `logic.deduct.validity_soundness`
- **Exercise Kinds:** `explain`, `choice`, `predict`
- **Core Mental Model:** Justified True Belief (JTB). Analysis of belief, truth conditions, and warrant.
- **Unseen Review Variants:** 3 variants classifying candidate beliefs against JTB criteria.

#### Lesson 4.2: Gettier Counterexamples & Defeasibility
- **Objective ID:** `phil.epistem.gettier`
- **Estimated Duration:** 7 minutes
- **Prerequisites:** `phil.epistem.jtb`
- **Exercise Kinds:** `choice`, `predict`, `order`
- **Core Mental Model:** Demonstrations that JTB is insufficient for knowledge: cases where justified belief coincides with truth purely by epistemic luck.
- **Unseen Review Variants:** 3 variants analyzing whether a given scenario constitutes a Gettier-type epistemic disconnect.

---

## 4. Track 3: Physics, Math & Engineering (`planned`)

*Status: Planned. Emphasizes first-principles physical derivations, conservation laws, state-space representations, and feedback systems.*

### Chapter 1: Classical Mechanics & Dynamic State Space
Focus: Kinematics, Newtonian dynamics, phase portraits, and conservation of momentum/energy.

#### Lesson 1.1: State Space and Phase Space Representation
- **Objective ID:** `phys.mech.state_space`
- **Estimated Duration:** 6 minutes
- **Prerequisites:** None
- **Exercise Kinds:** `explain`, `choice`, `predict`
- **Core Mental Model:** Dynamic system state represented as a vector $\mathbf{x}(t) = [x, \dot{x}]^T$ in phase space; trajectory governed by first-order coupled differential equations.
- **Unseen Review Variants:** 3 variants matching physical initial conditions to phase portrait trajectories.

#### Lesson 1.2: Conservation Principles as Invariants
- **Objective ID:** `phys.mech.conservation_laws`
- **Estimated Duration:** 7 minutes
- **Prerequisites:** `phys.mech.state_space`
- **Exercise Kinds:** `predict`, `choice`, `fill`
- **Core Mental Model:** Noether's theorem: continuous symmetries correspond to conserved quantities (time invariance $\to$ energy, spatial translation $\to$ momentum).
- **Unseen Review Variants:** 4 variants predicting post-collision states using conservation invariants alone.

---

### Chapter 2: Linear Systems, Signals & Frequency Domain
Focus: LTI systems, impulse response, convolution, Laplace/Fourier transforms.

#### Lesson 2.1: Linearity, Time-Invariance & Convolution
- **Objective ID:** `eng.lti.convolution`
- **Estimated Duration:** 7 minutes
- **Prerequisites:** `phys.mech.state_space`
- **Exercise Kinds:** `predict`, `order`, `choice`
- **Core Mental Model:** Superposition and shift invariance; system output completely characterized by the convolution of the input signal with the impulse response: $y(t) = (x * h)(t)$.
- **Unseen Review Variants:** 3 variants calculating system response to step and delta impulses.

#### Lesson 2.2: Transfer Functions and Pole-Zero Constellations
- **Objective ID:** `eng.lti.transfer_functions`
- **Estimated Duration:** 8 minutes
- **Prerequisites:** `eng.lti.convolution`
- **Exercise Kinds:** `choice`, `predict`, `fill`
- **Core Mental Model:** Laplace domain representation $H(s) = Y(s)/X(s)$; poles determine natural frequencies and damping; zeros shape transient attenuation.
- **Unseen Review Variants:** 4 variants deducing step response stability from pole locations in the complex $s$-plane.

---

### Chapter 3: Feedback Loops & Dynamic Stability
Focus: Closed-loop control, negative feedback, Nyquist/Bode stability criteria, PID control.

#### Lesson 3.1: The Principle of Negative Feedback
- **Objective ID:** `eng.ctrl.feedback_principle`
- **Estimated Duration:** 6 minutes
- **Prerequisites:** `eng.lti.transfer_functions`
- **Exercise Kinds:** `explain`, `predict`, `choice`
- **Core Mental Model:** Closed-loop transfer function $T(s) = \frac{G(s)}{1 + G(s)H(s)}$; trading forward-path gain for sensitivity reduction, disturbance rejection, and linear bandwidth expansion.
- **Unseen Review Variants:** 3 variants computing steady-state tracking error under parameter variations.

#### Lesson 3.2: Stability Margins: Gain and Phase Margins
- **Objective ID:** `eng.ctrl.stability_margins`
- **Estimated Duration:** 7 minutes
- **Prerequisites:** `eng.ctrl.feedback_principle`
- **Exercise Kinds:** `predict`, `choice`, `fill`
- **Core Mental Model:** Bode plots: gain margin (gain attenuation where phase is $-180^\circ$) and phase margin (phase advance where open-loop gain is $0\text{ dB}$). Instability boundary when open-loop loop gain reaches $-1 + j0$.
- **Unseen Review Variants:** 4 variants reading stability margins from asymptotic Bode diagrams.

#### Lesson 3.3: PID Controllers and Error Dynamics
- **Objective ID:** `eng.ctrl.pid_tuning`
- **Estimated Duration:** 8 minutes
- **Prerequisites:** `eng.ctrl.stability_margins`
- **Exercise Kinds:** `choice`, `order`, `predict`
- **Core Mental Model:**
  - Proportional ($K_p$): reduces rise time, introduces steady-state error.
  - Integral ($K_i$): eliminates steady-state offset by integrating history, introduces phase lag.
  - Derivative ($K_d$): anticipates error rate of change, adds damping, amplifies high-frequency noise.
- **Unseen Review Variants:** 4 variants predicting closed-loop step response changes upon adjusting individual gains.

---

### Chapter 4: Thermodynamics & Information Entropy
Focus: Statistical mechanics, macroscopic states, microstates, Shannon entropy, Landauer's bound.

#### Lesson 4.1: Microstates, Macrostates, and Boltzmann Entropy
- **Objective ID:** `phys.thermo.entropy_statmech`
- **Estimated Duration:** 6 minutes
- **Prerequisites:** `phys.mech.state_space`
- **Exercise Kinds:** `explain`, `choice`, `predict`
- **Core Mental Model:** $S = k_B \ln \Omega$. Multiplicity of microstates corresponding to an equilibrium macrostate; irreversibility as statistical progression toward maximal multiplicity.
- **Unseen Review Variants:** 3 variants calculating system multiplicity across state partitions.

#### Lesson 4.2: Shannon Information and Landauer's Erasure Principle
- **Objective ID:** `phys.info.shannon_landauer`
- **Estimated Duration:** 7 minutes
- **Prerequisites:** `phys.thermo.entropy_statmech`
- **Exercise Kinds:** `choice`, `predict`, `fill`
- **Core Mental Model:** Shannon entropy $H(X) = -\sum p(x) \log_2 p(x)$ as quantified uncertainty. Landauer's principle: irreversible logical erasure of 1 bit of information dissipates a minimum of $k_B T \ln 2$ heat into the environment.
- **Unseen Review Variants:** 3 variants analyzing information-theoretic limits on reversible computation and measurement.

---

## 5. Review Variant Matrix

| Objective ID | Domain | Base Lesson | Review Variant Count | Certification Requirement |
|---|---|---|---|---|
| `rust.mem.stack_layout` | Rust | 1.1 | 3 unseen variants | 1 unassisted pass @ 1d, 3d, 7d |
| `rust.own.move_semantics` | Rust | 1.2 | 4 unseen variants | 1 unassisted pass @ 1d, 3d, 7d, 14d |
| `rust.own.copy_vs_clone` | Rust | 1.3 | 3 unseen variants | 1 unassisted pass @ 1d, 3d, 7d |
| `rust.borrow.shared` | Rust | 2.1 | 3 unseen variants | 1 unassisted pass @ 1d, 3d, 7d |
| `rust.borrow.aliasing` | Rust | 2.2 | 4 unseen variants | 1 unassisted pass @ 1d, 3d, 7d, 14d |
| `rust.borrow.nll` | Rust | 2.3 | 3 unseen variants | 1 unassisted pass @ 1d, 3d, 7d |
| `rust.lifetime.elision` | Rust | 3.1 | 3 unseen variants | 1 unassisted pass @ 1d, 3d, 7d |
| `rust.lifetime.struct_bounds` | Rust | 3.2 | 4 unseen variants | 1 unassisted pass @ 1d, 3d, 7d, 14d |
| `rust.ptr.box` | Rust | 4.1 | 3 unseen variants | 1 unassisted pass @ 1d, 3d, 7d |
| `rust.ptr.rc_arc` | Rust | 4.2 | 3 unseen variants | 1 unassisted pass @ 1d, 3d, 7d |
| `rust.mut.interior` | Rust | 4.3 | 4 unseen variants | 1 unassisted pass @ 1d, 3d, 7d, 14d |
| `rust.concurrency.send_sync` | Rust | 4.4 | 4 unseen variants | 1 unassisted pass @ 1d, 3d, 7d, 14d |
| `logic.prop.atoms` | Logic | 1.1 | 3 unseen variants | 1 unassisted pass @ 1d, 3d |
| `logic.prop.connectives` | Logic | 1.2 | 4 unseen variants | 1 unassisted pass @ 1d, 3d, 7d |
| `logic.prop.conditional` | Logic | 1.3 | 4 unseen variants | 1 unassisted pass @ 1d, 3d, 7d, 14d |
| `logic.prop.biconditional` | Logic | 1.4 | 3 unseen variants | 1 unassisted pass @ 1d, 3d, 7d |
| `logic.deduct.validity_soundness` | Logic | 2.1 | 3 unseen variants | 1 unassisted pass @ 1d, 3d, 7d |
| `logic.deduct.modus_ponens_tollens` | Logic | 2.2 | 4 unseen variants | 1 unassisted pass @ 1d, 3d, 7d, 14d |
| `logic.deduct.syllogisms` | Logic | 2.3 | 3 unseen variants | 1 unassisted pass @ 1d, 3d, 7d |
| `logic.fallacy.affirming_consequent` | Logic | 3.1 | 3 unseen variants | 1 unassisted pass @ 1d, 3d, 7d, 14d |
| `logic.fallacy.denying_antecedent` | Logic | 3.2 | 3 unseen variants | 1 unassisted pass @ 1d, 3d, 7d, 14d |
| `logic.fallacy.undistributed_middle` | Logic | 3.3 | 3 unseen variants | 1 unassisted pass @ 1d, 3d, 7d |
| `phil.epistem.jtb` | Logic | 4.1 | 3 unseen variants | 1 unassisted pass @ 1d, 3d, 7d |
| `phil.epistem.gettier` | Logic | 4.2 | 3 unseen variants | 1 unassisted pass @ 1d, 3d, 7d |
| `phys.mech.state_space` | Physics | 1.1 | 3 unseen variants | 1 unassisted pass @ 1d, 3d, 7d |
| `phys.mech.conservation_laws` | Physics | 1.2 | 4 unseen variants | 1 unassisted pass @ 1d, 3d, 7d, 14d |
| `eng.lti.convolution` | Physics | 2.1 | 3 unseen variants | 1 unassisted pass @ 1d, 3d, 7d |
| `eng.lti.transfer_functions` | Physics | 2.2 | 4 unseen variants | 1 unassisted pass @ 1d, 3d, 7d, 14d |
| `eng.ctrl.feedback_principle` | Physics | 3.1 | 3 unseen variants | 1 unassisted pass @ 1d, 3d, 7d |
| `eng.ctrl.stability_margins` | Physics | 3.2 | 4 unseen variants | 1 unassisted pass @ 1d, 3d, 7d, 14d |
| `eng.ctrl.pid_tuning` | Physics | 3.3 | 4 unseen variants | 1 unassisted pass @ 1d, 3d, 7d, 14d |
| `phys.thermo.entropy_statmech` | Physics | 4.1 | 3 unseen variants | 1 unassisted pass @ 1d, 3d, 7d |
| `phys.info.shannon_landauer` | Physics | 4.2 | 3 unseen variants | 1 unassisted pass @ 1d, 3d, 7d |
