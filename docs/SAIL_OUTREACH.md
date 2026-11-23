# SAIL Outreach: find the conversations, draft the reply, I post it

**Status:** Proposed — nothing implemented · **Owners:** Discovery (watching, matching, proposals),
Bridge (read-only source adapters), Pulse (cadence, digest), Mind (model scheduling), Guardian
(network scopes) · **Related:** [Discovery](SAIL_DISCOVERY.md), [Proactive](SAIL_PROACTIVE.md),
[Pulse](SAIL_PULSE.md), [Bridge](SAIL_BRIDGE.md), [Studio](SAIL_STUDIO.md),
[Canvas](SAIL_CANVAS.md), [Telegram gateway](SAIL_TELEGRAM_GATEWAY.md) · **Cross-repo:** the
blog plan lives in `PedroVictorCoding/career-plan` (`05-blog.md`, track 5)

SAIL watches the communities where my blog's topics come up (Reddit, Discord, X, Hacker News,
Bluesky and others). When a thread is one where a post of mine genuinely helps, it drafts a reply
or a new post **in my voice**, and I decide whether to post it. It also turns each published blog
post into a podcast episode script, a video-essay script, and short-form versions.

"Human" here means the drafts read like me, a person who knows the topic and is joining a
conversation: they are useful on their own, and they link the blog only when the link answers the
question. It never means hiding who wrote them, posting from accounts that are not mine, or
faking engagement.

## 1. What the owner gets

| Where | What |
|---|---|
| **Morning digest** (Pulse, with the presence greeting) | At most 3 opportunities a day, ranked. Each card: the thread, why it matches, which post it relates to, the community's self-promotion rule, and a draft. |
| **Card actions** | Copy & open thread · Edit draft · Regenerate (shorter / no link / more technical) · Not now · Mute community · Mute topic · "Posted it" (feeds the link budget). |
| **Repurpose** (on each new blog post) | Podcast script, video-essay script with beat sheet, 3 short-clip scripts, an X/Bluesky thread, a self-contained Reddit text post. They open in Studio for editing. |
| **Voice** | "Anything worth replying to today?" → digest. "Turn the latest post into a podcast script" → repurpose job. |
| **Settings → Outreach** | Off by default. Watchlist (communities and keywords), daily cap, link budget, quiet hours (inherited from Pulse), per-source on/off. |

## 2. Hard rules

1. **Draft only in v1.** No Guardian capability to post, comment, vote, DM or join exists for
   Outreach. I post by hand. Any later "post for me" step needs its own spec, a per-post approval
   and an audit receipt, and it is off by default.
2. **My accounts only, disclosed.** No sockpuppets, no buying or trading engagement, no vote
   manipulation, no impersonation. When a reply links my own post, the draft says so ("I wrote
   about this here: …").
3. **Respect each community's rules.** Every watched community has a `self_promo` policy in its
   config: `allowed`, `weekly_thread_only`, `profile_link_only`, `forbidden` or `unknown`.
   `forbidden` communities get link-free drafts only; `unknown` is treated as forbidden until I set
   it. A common guideline is that most of what you post should not be self-promotion, so the
   default link budget is 1 link per community per week and 3 a week in total.
4. **Value first.** A draft must answer the thread on its own. The link is optional extra depth.
   The ranker drops matches where the only useful content would be the link.
5. **Platform terms decide the adapters.** Read only through official APIs, documented public
   feeds or bots that server admins invited. No scraping behind logins, no Discord self-bots or
   user tokens, no evading rate limits. Check each platform's current API terms and pricing before
   building its adapter, because they change.
6. **Research-section content rules carry over** from `career-plan/05-blog.md`: in medical or
   pharmacology threads, drafts summarize evidence and cite sources. They never give dosing advice
   or personal medical advice, and never describe personal drug use. Those threads are flagged for
   extra review.
7. **Minimal data.** Store thread URL, title, a short excerpt, timestamp and match score. No
   profiles of other users and no tracking of individuals. Opportunities expire after 14 days.

## 3. Sources (Bridge adapters, read-only)

| Source | Access | v1? |
|---|---|---|
| Reddit | Public subreddit/search RSS feeds first; the official Data API (OAuth app) only if RSS is not enough, after checking current access terms | Yes (RSS) |
| Hacker News | Official Firebase API and the Algolia search API | Yes |
| Bluesky / Mastodon | Open public APIs and hashtag/search feeds | Yes |
| Lobsters, dev.to, project forums | RSS | Optional |
| Discord | A bot account in my own server, or in servers whose admins invited it with read permission. Never a user account | Later |
| X | Official API; reading needs a paid tier, so verify pricing first. Fallback: skip X watching and only generate X threads in Repurpose | Later |
| LinkedIn | Already handled by the blog's own auto-post pipeline (career-plan track 5) | No |

Fetches run at `Background` priority on a Pulse schedule (default every 2 h, none in quiet hours),
with per-source rate limits below the published ones and conditional requests (ETag /
If-Modified-Since) where supported.

## 4. Flow

```text
 blog RSS / repo ─► Post index (title, tags, summary, key claims, embeddings)
                                   │
 Pulse timer ─► Bridge adapters ─► Raw threads ─► Filter (watchlist, language, age < 72 h, not seen)
                                                        │
                                                        ▼
                                Match: keyword prefilter → embedding similarity vs post index
                                                        │
                                                        ▼
            Score = relevance × freshness × answerability − promo risk − interruption cost
                                                        │  (Pulse attention; Drop / Digest / Suggest)
                                                        ▼
            Draft (resident model, Background priority; voice profile; community rules; link budget)
                                                        │
                                                        ▼
                  Self-check: answers on its own? within budget? rule-compliant? no medical advice?
                                                        │
                                                        ▼
                         Discovery card in the digest ─► owner feedback ─► learned preferences
```

- **Matching** is deterministic up to the draft step: keyword prefilter, then embeddings. The
  model only writes drafts for the top candidates, which keeps the 35B resident model's load small.
- **Voice profile.** Built from my published posts and the drafts I edited (the diff between draft
  and what I actually posted is the training signal). It records sentence length, vocabulary,
  how I open and close, and phrases I never use. Stored locally, editable in Settings.
- **Formats per platform.** Reddit: plain text, a direct answer first, a link at the end if any.
  HN: terse and technical, no marketing tone. Discord: short and conversational. X/Bluesky: a hook
  plus at most one link.

## 5. Repurpose: podcast and video-essay versions

Triggered when the blog repo publishes a post (RSS item or a push to main):

1. **Podcast script.** A spoken-style rewrite of 8 to 15 min: a cold-open hook, the question, the
   evidence, what I am unsure about, a call to read the post. Citations are said out loud ("a 2019
   meta-analysis of 30 trials found …"). Exact-wording audio uses the dedicated TTS path described
   in [Arena Learn](SAIL_ARENA_LEARN.md), but recording in my own voice is the default.
2. **Video-essay script.** The same structure plus a beat sheet: per beat, narration, an on-screen
   element (a Canvas diagram, a chart from the post's data, a code snippet, a paper screenshot with
   attribution) and a rough duration.
3. **Short clips.** Three scripts of 45 to 60 s, each built on one surprising finding from the
   post, for Shorts, Reels or TikTok.
4. **Thread and Reddit versions.** An X/Bluesky thread, and a self-contained Reddit text post that
   carries the full content natively, with the link at the end.

All outputs are Studio files that I edit. Rendering audio or video is a later slice. v1 produces
scripts and assets lists only, and never publishes.

## 6. Ownership and non-goals

Discovery owns detection, ranking and cards. Bridge owns adapters. Pulse owns cadence, the digest
and quiet hours. Mind schedules model work behind live voice. Guardian grants only read-only network
scopes per source. Studio holds repurposed scripts. Outreach does **not** post, track people, run
ad campaigns or grow follower counts by any means other than useful posts.

## 7. MVP vertical slice

Index the blog's RSS feed. Watch 5 subreddits through RSS plus HN keyword search. Produce one
morning digest with at most 3 drafted opportunities that respect each community's `self_promo`
setting and the link budget. "Posted it" updates the budget across a restart. Repurpose one
published post into a podcast script and a video-essay beat sheet in Studio.

### Acceptance checklist
- [ ] Confirm existing Discovery, Pulse, Bridge and Studio interfaces before adding code; reuse the Discovery card and Pulse digest.
- [ ] No posting capability is registered with Guardian for Outreach; a test asserts that.
- [ ] `unknown`/`forbidden` communities never get a draft containing a link (tested).
- [ ] Link budget holds across restart (tested).
- [ ] Rate limits and offline sources produce an honest "source unavailable" state, never stale data shown as fresh.
- [ ] Medical/pharmacology threads are flagged and their drafts pass the no-dosing-advice check.
- [ ] Measured model time per digest is recorded, and live voice pre-empts drafting.
- [ ] Focused tests and a reviewable PR without automatic merge.

## 8. Next steps
1. Decide the initial watchlist with the owner (communities, keywords, `self_promo` per community).
2. Read the current API terms for Reddit, X and Discord and record the outcome in this spec.
3. Build the MVP slice above; add Bluesky and Discord (own server) next.
4. Evaluate the voice profile: blind-compare drafts against my real posts before trusting it.
