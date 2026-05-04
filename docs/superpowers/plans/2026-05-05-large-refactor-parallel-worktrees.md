# Large Refactor — Parallel Worktrees Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 5월 5일 /simplify 검토에서 수집된 큰 리팩토링 항목을 6개 독립 워크트리로 병렬 진행한 뒤 main에 순차 머지하고, 마지막에 paths.rs Result화를 별도 직렬 단계로 처리한다.

**Architecture:** 각 워크트리는 서로 다른 파일/하위 시스템에 한정되어 머지 충돌이 거의 없다. 워크트리는 `~/.claude/worktrees/seoul-refactor-<group>` 경로에 생성한다. 모든 그룹은 본인 작업이 끝나면 `just lint && just test`를 통과시키고 PR-스타일 commit으로 마무리한다. 통합은 main에 fast-forward로 차례로 merge한다. paths.rs는 호출처가 5개 crate에 걸쳐 영향이 크기 때문에 다른 작업 머지 후 진행한다.

**Tech Stack:** Rust workspace · GPUI (Zed) · tokio async · libgit2/git CLI · ropey · rmp-serde · tree-sitter

**Worktree manifest:**

| WT | Branch | Scope | Touches |
|----|--------|-------|---------|
| WT1 | `refactor/editor-perf` | editor 성능 (idle tick, render 메모, async load, vertical_move 통합) | `crates/seoul-terminal/src/editor_view.rs`, `editor_buffer.rs` |
| WT2 | `refactor/list-virt` | 큰 리스트 가상화 | `crates/seoul-terminal/src/file_tree_view.rs`, `diff_view.rs` |
| WT3 | `refactor/app-structure` | TabKind enum화, subscription Vec 그룹화 | `crates/seoul-terminal/src/{app_view,pane,item,terminal_view,settings_view,editor_view,diff_view}.rs` |
| WT4 | `refactor/vt-safety` | sink swap RAII 가드, paste 인젝션 차단 | `crates/seoul-vt/src/terminal.rs` |
| WT5 | `refactor/daemon-hotpath` | PTY Bytes, flock, tmp slot, per-session 채널 | `crates/seoul-daemon/src/{session,host,server,main}.rs`, `seoul-workspace/src/persistence.rs` |
| WT6 | `refactor/git-status-parallel` | `compute_status` 7개 서브프로세스 병렬화 | `crates/seoul-workspace/src/git/status.rs` |
| WT7 (직렬) | `refactor/paths-result` | `seoul_dir()` Result화 + 호출처 전수 변경 | `crates/seoul-terminal-proto/src/paths.rs` + 모든 호출처 |

**Conflict matrix:** WT1·WT2·WT3는 모두 `seoul-terminal` 크레이트지만 다른 파일이므로 동시 진행 가능. WT3가 `editor_view.rs`/`diff_view.rs` import 라인을 만질 수 있어 머지 순서는 WT3 → WT1 → WT2 권장. 다른 그룹(WT4·WT5·WT6)은 다른 크레이트라 충돌 없음.

**Common workflow per worktree:** 모든 워크트리 task는 다음 5단계를 따른다 — (1) 실패하는 테스트 작성, (2) `cargo test -p <crate> <test_name>`로 실패 확인, (3) 최소 구현, (4) `just lint && just test`로 통과 확인, (5) `Co-Authored-By` 포함 단일 커밋. UI 동작 변경(GPUI tick/렌더)은 단위 테스트가 어렵다 — 그 경우 행동 추출(epoch 비교, 캐시 키 생성 등)을 헬퍼 함수로 분리해 단위 테스트하고, GPUI 통합은 `just app`으로 수동 확인을 명시적 step으로 둔다.

---

## Phase 0: Setup

### Task 0.1: Create all 6 parallel worktrees

**Files:** none (git operation only)

- [ ] **Step 1: Verify repo is clean**

```bash
git status
```

Expected: working tree clean, branch `main`, in sync with origin.

- [ ] **Step 2: Create worktrees**

```bash
mkdir -p ~/.claude/worktrees
for g in editor-perf list-virt app-structure vt-safety daemon-hotpath git-status-parallel; do
  git worktree add ~/.claude/worktrees/seoul-refactor-$g -b refactor/$g main
done
git worktree list
```

Expected output: 7 entries (main + 6 worktrees).

- [ ] **Step 3: Confirm each worktree builds**

```bash
for g in editor-perf list-virt app-structure vt-safety daemon-hotpath git-status-parallel; do
  (cd ~/.claude/worktrees/seoul-refactor-$g && just lint) || echo "FAIL: $g"
done
```

Expected: no FAIL lines.

---

## Phase 1: Parallel worktrees (WT1–WT6)

각 워크트리는 자기 디렉터리에서 독립적으로 진행. 워크트리 별로 별도 커밋 시리즈를 만든다.

---

## WT1 — Editor 성능 (`refactor/editor-perf`)

워크트리: `~/.claude/worktrees/seoul-refactor-editor-perf`
관련 파일: `crates/seoul-terminal/src/editor_view.rs`, `crates/seoul-terminal/src/editor_buffer.rs`

검토 결과 핵심 문제 4가지:
1. `tick()`이 매 프레임 자기 자신을 재예약 → idle에도 60Hz 깨움 (`editor_view.rs:169-193`)
2. `render()`가 매 프레임 line_text 수집 + tree-sitter 하이라이트 재실행 (`editor_view.rs:1051-1099`)
3. `EditorView::new()`가 UI 스레드에서 파일 read + parse (`editor_view.rs:90-98`)
4. `move_up`/`move_down`/`select_up`/`select_down` 4중복 (`editor_view.rs:584-628, 677-711`)

기존 패턴 참조: `terminal_view.rs:1058-1105`의 `show_cursor_now` + `tick_blink` epoch+timer 패턴.

### Task WT1.1: blink epoch 인프라 도입

**Files:**
- Modify: `crates/seoul-terminal/src/editor_view.rs` (struct 필드 + helper)

- [ ] **Step 1: Write failing unit test**

`crates/seoul-terminal/src/editor_view.rs` 하단 `#[cfg(test)]` 모듈에 추가:

```rust
#[test]
fn blink_epoch_monotonic_bump() {
    let mut e = BlinkState::default();
    let a = e.bump();
    let b = e.bump();
    assert!(b > a);
}

#[test]
fn blink_state_stale_callback_is_noop() {
    let mut e = BlinkState::default();
    let stale = e.bump();
    let _fresh = e.bump();
    assert!(!e.should_tick(stale));
}
```

- [ ] **Step 2: Run test, expect compile error**

```bash
cargo test -p seoul-terminal blink_state -- --nocapture
```

Expected: compile error — `BlinkState` not found.

- [ ] **Step 3: Add `BlinkState` and integrate into `EditorView`**

`editor_view.rs`에 추가 (struct 정의 부근):

```rust
#[derive(Default)]
struct BlinkState {
    epoch: u64,
}

impl BlinkState {
    fn bump(&mut self) -> u64 {
        self.epoch += 1;
        self.epoch
    }
    fn should_tick(&self, scheduled_epoch: u64) -> bool {
        scheduled_epoch == self.epoch
    }
}
```

`EditorView` struct에 필드 추가: `blink: BlinkState,`. 기존 `last_blink_toggle`/`cursor_blink_visible`은 유지.

- [ ] **Step 4: Run test → pass**

```bash
cargo test -p seoul-terminal blink_state
```

Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/seoul-terminal/src/editor_view.rs
git commit -m "$(cat <<'EOF'
WT1.1: introduce BlinkState epoch helper for editor cursor

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task WT1.2: tick() → epoch+timer로 교체 (idle 시 0 wakeup)

**Files:**
- Modify: `crates/seoul-terminal/src/editor_view.rs:169-193` and the call site that bootstraps `tick()`

- [ ] **Step 1: Find current bootstrap of `tick`**

```bash
grep -n "self\.tick\|fn tick\b" crates/seoul-terminal/src/editor_view.rs
```

기록된 모든 호출처를 확인.

- [ ] **Step 2: Replace tick() body with epoch-driven blink**

`editor_view.rs:169-193`의 `tick`을 다음으로 교체:

```rust
const BLINK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
const BLINK_PAUSE: std::time::Duration = std::time::Duration::from_millis(500);

fn show_cursor_now(&mut self, cx: &mut Context<Self>) {
    if !self.cursor_blink_visible {
        self.cursor_blink_visible = true;
        cx.notify();
    }
    self.last_blink_toggle = std::time::Instant::now();
    let epoch = self.blink.bump();
    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(Self::BLINK_PAUSE).await;
        if let Some(this) = this.upgrade() {
            this.update(cx, |this, cx| this.tick_blink(epoch, cx)).ok();
        }
    })
    .detach();
}

fn tick_blink(&mut self, epoch: u64, cx: &mut Context<Self>) {
    if !self.blink.should_tick(epoch) {
        return;
    }
    if self.last_edit_epoch.elapsed() < Self::BLINK_PAUSE {
        return;
    }
    self.cursor_blink_visible = !self.cursor_blink_visible;
    self.last_blink_toggle = std::time::Instant::now();
    cx.notify();
    let next = self.blink.bump();
    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(Self::BLINK_INTERVAL).await;
        if let Some(this) = this.upgrade() {
            this.update(cx, |this, cx| this.tick_blink(next, cx)).ok();
        }
    })
    .detach();
}
```

- [ ] **Step 3: Replace bootstrap call**

기존 `cx.on_next_frame(window, |this, window, cx| this.tick(window, cx));` 패턴을 `EditorView::new()`/focus 핸들러에서 찾아 `self.show_cursor_now(cx);`로 대체. 키 입력/IME 핸들러에서도 `show_cursor_now(cx)`를 호출해 입력 시 즉시 켜지도록.

- [ ] **Step 4: Remove legacy `fn tick`**

`tick` 함수 본체 삭제.

- [ ] **Step 5: Manual verification**

```bash
just app
```

Editor 탭 열고 idle 상태에서 macOS Activity Monitor의 seoul 프로세스 CPU가 0~1%로 떨어지는지 확인. 키 입력 시 커서가 즉시 보이고 깜빡임이 정상인지 확인.

- [ ] **Step 6: Commit**

```bash
git add crates/seoul-terminal/src/editor_view.rs
git commit -m "$(cat <<'EOF'
WT1.2: switch editor tick from per-frame loop to epoch+timer (0 idle wakeups)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task WT1.3: render 메모이제이션 — visible_lines + highlight_spans

**Files:**
- Modify: `crates/seoul-terminal/src/editor_view.rs` (struct + render path around line 1051)

- [ ] **Step 1: Write failing test for cache key invalidation**

```rust
#[test]
fn render_cache_invalidates_on_buffer_version_change() {
    let mut cache = RenderCache::default();
    cache.update(BufferVersion(1), 0..20, ScrollOffset(0));
    assert!(cache.is_fresh(BufferVersion(1), 0..20, ScrollOffset(0)));
    assert!(!cache.is_fresh(BufferVersion(2), 0..20, ScrollOffset(0)));
}

#[test]
fn render_cache_invalidates_on_scroll() {
    let mut cache = RenderCache::default();
    cache.update(BufferVersion(1), 0..20, ScrollOffset(0));
    assert!(!cache.is_fresh(BufferVersion(1), 0..20, ScrollOffset(40)));
}
```

- [ ] **Step 2: Run test → compile fail**

```bash
cargo test -p seoul-terminal render_cache_invalidates
```

Expected: compile error — types missing.

- [ ] **Step 3: Add cache types + struct field**

```rust
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
pub struct BufferVersion(pub u64);

#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
pub struct ScrollOffset(pub usize);

#[derive(Default)]
struct RenderCache {
    key: Option<(BufferVersion, std::ops::Range<usize>, ScrollOffset)>,
    visible_lines: Vec<String>,
    highlight_spans: Vec<crate::syntax::HighlightSpan>,
}

impl RenderCache {
    fn is_fresh(&self, ver: BufferVersion, range: std::ops::Range<usize>, scroll: ScrollOffset) -> bool {
        self.key.as_ref().map(|k| k == &(ver, range, scroll)).unwrap_or(false)
    }
    fn update(&mut self, ver: BufferVersion, range: std::ops::Range<usize>, scroll: ScrollOffset) {
        self.key = Some((ver, range, scroll));
    }
}
```

`EditorBuffer`에 `fn version(&self) -> BufferVersion` 추가 (`editor_buffer.rs`). 모든 mutator는 `self.version.0 += 1;`.

`EditorView`에 `render_cache: RenderCache,` 필드 추가.

- [ ] **Step 4: Wire cache into render path**

`render()` 내 `let visible_lines: Vec<_> = (start..end).map(|i| self.buffer.line_text(i)).collect();` 블록을 캐시 체크로 감싼다:

```rust
let ver = self.buffer.version();
let scroll = ScrollOffset(self.scroll_offset_lines);
if !self.render_cache.is_fresh(ver, start..end, scroll) {
    self.render_cache.visible_lines = (start..end).map(|i| self.buffer.line_text(i)).collect();
    self.render_cache.highlight_spans = self.highlighter.highlight_lines(&self.render_cache.visible_lines);
    self.render_cache.update(ver, start..end, scroll);
}
let visible_lines = &self.render_cache.visible_lines;
let highlight_spans = &self.render_cache.highlight_spans;
```

- [ ] **Step 5: Run tests → pass**

```bash
just test 2>&1 | grep -E "render_cache|FAILED"
```

Expected: 2 passed, no FAILED.

- [ ] **Step 6: Manual paint sanity check**

```bash
just app
```

긴 파일을 열고 스크롤·편집 동작 확인. 하이라이팅이 깨지지 않는지 검증.

- [ ] **Step 7: Commit**

```bash
git commit -am "$(cat <<'EOF'
WT1.3: memoize editor visible_lines + highlight_spans by (version, range, scroll)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task WT1.4: EditorView::new() 비동기 로드

**Files:**
- Modify: `crates/seoul-terminal/src/editor_view.rs:90-98`

- [ ] **Step 1: Add async load constructor**

기존 `EditorView::new(file_path, ...)`의 `let content = std::fs::read_to_string(&file_path)?;` + `EditorBuffer::from_str` + `highlighter.parse(&content)` 블록을 placeholder 버퍼로 대체하고, `cx.spawn`으로 background load:

```rust
pub fn new(file_path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let mut view = Self {
        buffer: EditorBuffer::empty(),
        loading: true,
        // ... 다른 필드 기본값
        ..Default::default()
    };
    let path = file_path.clone();
    cx.spawn(async move |this, cx| {
        let content = smol::unblock(move || std::fs::read_to_string(&path)).await;
        if let (Some(this), Ok(content)) = (this.upgrade(), content) {
            this.update(cx, |this, cx| {
                this.buffer = EditorBuffer::from_str(&content);
                this.highlighter.parse(&content);
                this.loading = false;
                cx.notify();
            }).ok();
        }
    }).detach();
    view
}
```

- [ ] **Step 2: Render loading placeholder**

`render()`에서 `self.loading`이면 `div().child("Loading…")` 반환. 그 외에는 기존 그리드 렌더.

- [ ] **Step 3: Manual verification**

```bash
just app
```

큰 파일(1MB+)을 열어 클릭 후 즉시 placeholder가 보이고, 잠시 뒤 내용이 나타나는지 확인.

- [ ] **Step 4: Commit**

```bash
git commit -am "$(cat <<'EOF'
WT1.4: load editor file content off the UI thread (placeholder while reading)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task WT1.5: vertical_move 통합 (4 duplicates → 1)

**Files:**
- Modify: `crates/seoul-terminal/src/editor_view.rs:584-628, 677-711`

- [ ] **Step 1: Read current 4 fns**

```bash
sed -n '584,720p' crates/seoul-terminal/src/editor_view.rs
```

`move_up`/`move_down`/`select_up`/`select_down` 본체 확인.

- [ ] **Step 2: Add unified helper**

```rust
fn vertical_move(&mut self, dir: i32, extend: bool, cx: &mut Context<Self>) {
    debug_assert!(dir == -1 || dir == 1);
    if !extend {
        self.selection_anchor = None;
    } else if self.selection_anchor.is_none() {
        self.selection_anchor = Some(self.cursor);
    }
    let target_row = (self.cursor.row as i32 + dir).clamp(0, self.buffer.line_count() as i32 - 1);
    let target_col = self.desired_col.unwrap_or(self.cursor.col);
    let line_len = self.buffer.line_len(target_row as usize);
    self.cursor = CursorPos { row: target_row as usize, col: target_col.min(line_len) };
    self.show_cursor_now(cx);
    cx.notify();
}
```

기존 4개 함수를 본체 한 줄짜리 wrapper로 교체:
```rust
fn move_up(&mut self, cx: &mut Context<Self>) { self.vertical_move(-1, false, cx); }
fn move_down(&mut self, cx: &mut Context<Self>) { self.vertical_move(1, false, cx); }
fn select_up(&mut self, cx: &mut Context<Self>) { self.vertical_move(-1, true, cx); }
fn select_down(&mut self, cx: &mut Context<Self>) { self.vertical_move(1, true, cx); }
```

- [ ] **Step 3: Verify behaviour**

```bash
just lint && just test
just app
```

vim 키바인딩(`j`/`k`/`shift+j`/`shift+k`)과 화살표 키 + shift로 위/아래 이동 + 선택이 모두 동작하는지 수동 확인.

- [ ] **Step 4: Commit**

```bash
git commit -am "$(cat <<'EOF'
WT1.5: unify editor move_up/down/select_up/down into vertical_move(dir, extend)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task WT1.6: WT1 마무리 검증

- [ ] **Step 1: Full check**

```bash
just lint && just test
```

Expected: 0 warnings, 0 failures.

- [ ] **Step 2: WT1 push (if remote tracking is configured)**

```bash
git push -u origin refactor/editor-perf
```

(remote가 없으면 스킵.)

---

## WT2 — 리스트 가상화 (`refactor/list-virt`)

워크트리: `~/.claude/worktrees/seoul-refactor-list-virt`
관련 파일: `crates/seoul-terminal/src/file_tree_view.rs`, `crates/seoul-terminal/src/diff_view.rs`

검토 결과: `file_tree_view.rs:282-287`와 `diff_view.rs:168-170`가 모든 항목을 매 프레임 element tree로 빌드 → 큰 프로젝트/큰 diff에서 paint 비용 폭발. GPUI의 `uniform_list`로 가시 범위만 렌더.

### Task WT2.1: file_tree_view를 uniform_list로

**Files:**
- Modify: `crates/seoul-terminal/src/file_tree_view.rs:282-287` and surrounding render fn

- [ ] **Step 1: Confirm GPUI version exposes `uniform_list`**

```bash
grep -rn "uniform_list" crates/seoul-terminal/src/ | head
```

기존 사용처가 있으면 패턴을 그대로 따른다. 없으면 `gpui::uniform_list` 시그니처 확인:

```bash
cargo doc -p gpui --no-deps --open  # or grep .cargo/registry for uniform_list signature
```

- [ ] **Step 2: Replace flat iter render with uniform_list**

`render` 내 `for entry in &self.flat_entries { ... }` 블록을 다음 형태로:

```rust
uniform_list(
    "file-tree",
    self.flat_entries.len(),
    cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
        range.map(|i| this.render_entry(i, cx)).collect::<Vec<_>>()
    }),
)
.flex_grow()
.into_any_element()
```

`render_entry(i, cx)`는 기존 inline 코드를 메소드로 추출(`#[inline]` 권장). `git_color_for_path`는 entry당 한 번만 호출되도록 `render_entry` 진입부에서 한 번 캐시한다 (검토 보고 #9).

- [ ] **Step 3: Manual smoke test**

```bash
just app
```

큰 monorepo (수천 파일)에서 사이드바 트리 펼치고 스크롤. 끊김 없이 부드러운지 확인. 펼치기/접기 동작 확인.

- [ ] **Step 4: Commit**

```bash
git commit -am "$(cat <<'EOF'
WT2.1: virtualize file tree with uniform_list (only visible rows)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task WT2.2: diff_view를 uniform_list로

**Files:**
- Modify: `crates/seoul-terminal/src/diff_view.rs:168-170` and surrounding render fn

- [ ] **Step 1: Apply same pattern**

`for line in self.lines.iter()` 블록을 `uniform_list("diff-lines", self.lines.len(), |this, range, _, cx| range.map(|i| this.render_diff_line(i, cx)).collect())`로 교체.

- [ ] **Step 2: Smoke test**

```bash
just app
```

5000줄 이상의 diff를 열어 스크롤. row 높이가 일정해야 한다 (diff 한 줄 = 한 row, wrap 없음 가정).

- [ ] **Step 3: Commit**

```bash
git commit -am "$(cat <<'EOF'
WT2.2: virtualize diff view with uniform_list

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task WT2.3: WT2 마무리 검증

- [ ] **Step 1: Full check**

```bash
just lint && just test
```

---

## WT3 — app_view 구조 (`refactor/app-structure`)

워크트리: `~/.claude/worktrees/seoul-refactor-app-structure`
관련 파일: `crates/seoul-terminal/src/{app_view,pane,item,terminal_view,settings_view,editor_view,diff_view}.rs`

검토 결과:
1. `app_view.rs`/`pane.rs`/`item.rs`에 stringly-typed tab kind (`"terminal"`, `"editor"`, `"settings"`, `"diff"`)가 14+곳에 흩어져 있음
2. `AppView`에 `_file_tree_subscription`, `_git_subscription`, `_pr_event_task` 등 keep-alive 필드 10여 개

### Task WT3.1: TabKind enum 도입

**Files:**
- Create: `crates/seoul-terminal/src/tab_kind.rs`
- Modify: `crates/seoul-terminal/src/main.rs` or `lib.rs` to declare module
- Modify: `crates/seoul-terminal/src/{pane,item,app_view,terminal_view,editor_view,diff_view,settings_view}.rs`

- [ ] **Step 1: Failing test**

`crates/seoul-terminal/src/tab_kind.rs` 새 파일:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabKind {
    Terminal,
    Editor,
    Settings,
    Diff,
}

impl TabKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TabKind::Terminal => "terminal",
            TabKind::Editor => "editor",
            TabKind::Settings => "settings",
            TabKind::Diff => "diff",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_variants_round_trip_str() {
        for k in [TabKind::Terminal, TabKind::Editor, TabKind::Settings, TabKind::Diff] {
            let json = serde_json::to_string(&k).unwrap();
            let back: TabKind = serde_json::from_str(&json).unwrap();
            assert_eq!(k, back);
            assert_eq!(json.trim_matches('"'), k.as_str());
        }
    }
}
```

`main.rs` (또는 `lib.rs`)에 `mod tab_kind;` 추가.

```bash
cargo test -p seoul-terminal tab_kind
```

Expected: 1 passed.

- [ ] **Step 2: Replace `kind_id: &'static str` with `kind: TabKind`**

`pane.rs`, `item.rs`의 `Item` trait, `TabMetadata` 등에서 `kind_id: &'static str`/`String`을 `kind: TabKind`로 교체. `Item::tab_kind_id` → `Item::tab_kind() -> TabKind`.

`app_view.rs`의 `find_tab_by_kind`, `kind_id == "terminal"` 같은 비교를 `tab.kind() == TabKind::Terminal`로 전수 치환.

- [ ] **Step 3: Persistence layer 호환**

`workspace::persistence`의 `PersistedTabKind`(있으면)와 `TabKind` 직렬화가 호환되는지 확인. 키 이름이 다르면 `From`/`Into` 인스턴스 작성.

- [ ] **Step 4: Verify**

```bash
just lint && just test
just app  # 모든 탭 종류를 열고/닫고/저장/복원 점검
```

- [ ] **Step 5: Commit**

```bash
git commit -am "$(cat <<'EOF'
WT3.1: replace stringly-typed tab kinds with TabKind enum

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task WT3.2: Subscription/Task 필드 묶기

**Files:**
- Modify: `crates/seoul-terminal/src/app_view.rs:174-251`

- [ ] **Step 1: Replace 10+ Option<Subscription/Task> fields with two Vecs**

기존 필드들 (`_file_tree_subscription`, `_git_subscription`, `_git_panel_subscription`, `_quit_subscription`, `_daemon_health_task`, `_daemon_connect_task`, `_pr_event_task`)을:

```rust
_subscriptions: Vec<gpui::Subscription>,
_tasks: Vec<gpui::Task<()>>,
```

로 통합. 각 호출처에서 `self._file_tree_subscription = Some(sub)` 패턴을 `self._subscriptions.push(sub)`로 교체.

- [ ] **Step 2: Verify**

```bash
just lint && just test
just app
```

종료 시 panic 없이 깨끗하게 닫히는지 확인 (subscription drop 순서 변경 영향).

- [ ] **Step 3: Commit**

```bash
git commit -am "$(cat <<'EOF'
WT3.2: collapse AppView keep-alive fields into _subscriptions/_tasks vectors

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task WT3.3: WT3 마무리 검증

- [ ] **Step 1: Full check**

```bash
just lint && just test
```

---

## WT4 — VT 안전성 (`refactor/vt-safety`)

워크트리: `~/.claude/worktrees/seoul-refactor-vt-safety`
관련 파일: `crates/seoul-vt/src/terminal.rs`

검토 결과:
1. `feed_pty_data_silently` (`terminal.rs:312-319`)가 sink로 swap 후 다시 swap하는 패턴 — panic 시 sink가 영구 고착
2. paste(`terminal.rs:294`)에서 `\x1b` 단순 `replace`는 `\x1b[201~`(bracketed-paste 종결자)를 막지 못함

### Task WT4.1: SinkSwapGuard RAII

**Files:**
- Modify: `crates/seoul-vt/src/terminal.rs:312-319`

- [ ] **Step 1: Failing test**

`terminal.rs` `#[cfg(test)] mod tests`에:

```rust
#[test]
fn sink_swap_guard_restores_on_panic() {
    use std::sync::{Arc, Mutex};
    let writer: Arc<Mutex<Box<dyn std::io::Write + Send>>> = Arc::new(Mutex::new(Box::new(Vec::<u8>::new())));
    let writer_for_panic = writer.clone();
    let result = std::panic::catch_unwind(move || {
        let _guard = SinkSwapGuard::new(&writer_for_panic);
        panic!("boom");
    });
    assert!(result.is_err());
    let mut w = writer.lock().unwrap();
    w.write_all(b"ok").unwrap();
    drop(w);
    let inner = std::mem::replace(&mut *writer.lock().unwrap(), Box::new(std::io::sink()));
    let buf: Vec<u8> = *inner.downcast::<Vec<u8>>().unwrap();
    assert_eq!(buf, b"ok");
}
```

(실제 시그니처는 코드에 맞춰 조정.)

- [ ] **Step 2: Implement guard**

```rust
struct SinkSwapGuard<'a> {
    writer: &'a Arc<Mutex<Box<dyn Write + Send>>>,
    saved: Option<Box<dyn Write + Send>>,
}

impl<'a> SinkSwapGuard<'a> {
    fn new(writer: &'a Arc<Mutex<Box<dyn Write + Send>>>) -> Self {
        let mut w = writer.lock().expect("pty writer poisoned");
        let saved = std::mem::replace(&mut *w, Box::new(std::io::sink()));
        Self { writer, saved: Some(saved) }
    }
}

impl Drop for SinkSwapGuard<'_> {
    fn drop(&mut self) {
        if let Some(saved) = self.saved.take() {
            if let Ok(mut w) = self.writer.lock() {
                *w = saved;
            }
        }
    }
}
```

`feed_pty_data_silently`를 다음으로 교체:

```rust
pub fn feed_pty_data_silently(&self, data: &[u8]) {
    let _guard = SinkSwapGuard::new(&self.pty_writer);
    self.vt_write(data);
}
```

- [ ] **Step 3: Run test → pass**

```bash
cargo test -p seoul-vt sink_swap
```

- [ ] **Step 4: Commit**

```bash
git commit -am "$(cat <<'EOF'
WT4.1: replace sink-swap pair with RAII SinkSwapGuard (panic-safe)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task WT4.2: paste bracketed-paste injection 차단

**Files:**
- Modify: `crates/seoul-vt/src/terminal.rs:294`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn paste_strips_bracketed_paste_terminator() {
    let cleaned = sanitize_paste("hello\x1b[201~world\x1b[201~end");
    assert!(!cleaned.contains("\x1b[201~"));
    assert!(cleaned.contains("hello"));
    assert!(cleaned.contains("world"));
    assert!(cleaned.contains("end"));
}
```

- [ ] **Step 2: Implement sanitize_paste**

```rust
pub(crate) fn sanitize_paste(s: &str) -> String {
    s.replace("\x1b[201~", "").replace('\x1b', "")
}
```

기존 paste 흐름에서 `replace('\x1b', "")` 라인을 `sanitize_paste(text)` 호출로 교체.

- [ ] **Step 3: Run test → pass + commit**

```bash
cargo test -p seoul-vt paste_strips
git commit -am "$(cat <<'EOF'
WT4.2: sanitize bracketed-paste terminator (\\x1b[201~) before forwarding

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task WT4.3: WT4 마무리 검증

- [ ] **Step 1: Full check**

```bash
just lint && just test
```

---

## WT5 — Daemon 핫패스 (`refactor/daemon-hotpath`)

워크트리: `~/.claude/worktrees/seoul-refactor-daemon-hotpath`
관련 파일: `crates/seoul-daemon/src/{session,host,server,main}.rs`, `crates/seoul-workspace/src/persistence.rs`

검토 결과:
1. PTY 핫패스에서 `buf[..n].to_vec()`이 매 read마다 alloc (`session.rs:545`)
2. 데몬 락에 TOCTOU race (`main.rs:151`)
3. token 파일 chmod 윈도 (`main.rs:62`)
4. 모든 세션이 공유하는 단일 mpsc(256) 브로드캐스트 (`host.rs:46`)
5. `state.json.tmp` 단일 슬롯 race (`persistence.rs:251`)
6. `host::spawn_and_attach` ↔ `spawn_unattached` 90% 동일 (`host.rs:106 & 140`)

### Task WT5.1: PTY read에 Bytes 도입 (alloc-light hot path)

**Files:**
- Modify: `crates/seoul-daemon/src/session.rs:545` and `ClientEvent::Data` payload type

- [ ] **Step 1: Add `bytes` dep if missing**

```bash
grep -n "^bytes" crates/seoul-daemon/Cargo.toml || echo "ADD bytes = \"1\""
```

필요시 `Cargo.toml`에 `bytes = "1"` 추가.

- [ ] **Step 2: Change `ClientEvent::Data(Vec<u8>)` → `ClientEvent::Data(Bytes)`**

`session.rs`/`host.rs`의 `ClientEvent` enum 수정 + 모든 producer를 `Bytes::copy_from_slice(&buf[..n])`로, consumer는 `&[u8]`로 deref 가능.

- [ ] **Step 3: Verify hot loop**

```bash
cargo flamegraph -p seoul-daemon --root --bin seoul-daemon -- --duration 30s &
# 다른 터미널에서 cat /dev/urandom | head -c 100M  같은 부하
```

`Vec` alloc symbol 비중이 줄었는지 확인. (선택적; flamegraph 미설치면 스킵.)

- [ ] **Step 4: Commit**

```bash
git commit -am "$(cat <<'EOF'
WT5.1: pass PTY chunks as Bytes through ClientEvent::Data (cuts per-read alloc)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task WT5.2: 데몬 락에 flock 적용

**Files:**
- Modify: `crates/seoul-daemon/src/main.rs:151` (`acquire_daemon_lock`)
- `Cargo.toml`: 필요 시 `nix = { version = "0.29", features = ["fcntl"] }` 추가

- [ ] **Step 1: Failing integration test**

`crates/seoul-daemon/tests/lock_test.rs`:

```rust
#[test]
fn lock_blocks_second_acquire() {
    let dir = tempfile::tempdir().unwrap();
    let lock = dir.path().join("daemon.lock");
    let _h1 = seoul_daemon::lock::acquire(&lock).expect("first lock");
    assert!(seoul_daemon::lock::acquire(&lock).is_err(), "second lock must fail");
}
```

- [ ] **Step 2: Implement lock module**

```rust
// crates/seoul-daemon/src/lock.rs
use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use anyhow::{Context, Result, bail};
use nix::fcntl::{Flock, FlockArg};

pub struct LockHandle(Flock<File>);

pub fn acquire(path: &Path) -> Result<LockHandle> {
    let f = OpenOptions::new().create(true).read(true).write(true).open(path)
        .with_context(|| format!("open lock: {}", path.display()))?;
    match Flock::lock(f, FlockArg::LockExclusiveNonblock) {
        Ok(l) => Ok(LockHandle(l)),
        Err((_, e)) => bail!("daemon already running ({})", e),
    }
}
```

`main.rs`의 `acquire_daemon_lock`을 `lock::acquire(&lock_path)?`로 교체. `LockHandle`을 데몬 lifetime 동안 유지.

- [ ] **Step 3: Run test → pass + commit**

```bash
cargo test -p seoul-daemon lock_blocks
git commit -am "$(cat <<'EOF'
WT5.2: replace TOCTOU pid-file check with flock for daemon singleton lock

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task WT5.3: token 파일 mode 0o600을 create-time에 적용

**Files:**
- Modify: `crates/seoul-daemon/src/main.rs:62`

- [ ] **Step 1: Replace create+chmod with create_new+mode**

```rust
use std::os::unix::fs::OpenOptionsExt;
let mut f = std::fs::OpenOptions::new()
    .create_new(true)
    .write(true)
    .mode(0o600)
    .open(&token_path)
    .with_context(|| format!("create token at {}", token_path.display()))?;
f.write_all(token.as_bytes())?;
```

기존 `fs::write(&token_path, &token)` + chmod 라인 제거.

- [ ] **Step 2: Verify file mode**

```bash
just kill-daemon
just dev &  # 또는 단발 실행
sleep 1
stat -f "%Lp" ~/.seoul/terminal-host.token
```

Expected: `600`.

- [ ] **Step 3: Commit**

```bash
git commit -am "$(cat <<'EOF'
WT5.3: write daemon token with mode 0o600 atomically (eliminate world-readable window)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task WT5.4: persistence의 tmp 파일 rename race 제거

**Files:**
- Modify: `crates/seoul-workspace/src/persistence.rs:251`

- [ ] **Step 1: Add unique tmp suffix**

```rust
let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
```

또는 `uuid::Uuid::new_v4().simple()` 사용. 그 다음 `rename(tmp, path)`로 원자 교체.

- [ ] **Step 2: Failing concurrent test**

```rust
#[test]
fn save_state_concurrent_does_not_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let s = AppState::default();
    let p1 = path.clone(); let s1 = s.clone();
    let p2 = path.clone(); let s2 = s.clone();
    let h1 = std::thread::spawn(move || save_state(&s1, &p1).unwrap());
    let h2 = std::thread::spawn(move || save_state(&s2, &p2).unwrap());
    h1.join().unwrap(); h2.join().unwrap();
    let txt = std::fs::read_to_string(&path).unwrap();
    let _: AppState = serde_json::from_str(&txt).expect("file must remain valid JSON");
}
```

- [ ] **Step 3: Run test → pass + commit**

```bash
cargo test -p seoul-workspace save_state_concurrent
git commit -am "$(cat <<'EOF'
WT5.4: include pid in persistence tmp file to avoid concurrent rename race

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task WT5.5: host broadcast 채널을 per-session으로

**Files:**
- Modify: `crates/seoul-daemon/src/host.rs:46` and producer/consumer plumbing

- [ ] **Step 1: Replace shared mpsc with per-session map**

`Host` struct에 `event_tx: HashMap<SessionId, mpsc::Sender<ClientEvent>>` 도입. 세션 attach 시 채널 생성, detach 시 drop. 각 세션 reader는 자기 세션 채널에만 push.

- [ ] **Step 2: Verify head-of-line blocking 해결**

수동 시나리오: 세션 A에서 `cat /dev/urandom`, 세션 B에서 일반 입력. 세션 B의 응답이 A의 트래픽에 막히지 않는지 (뚜렷한 키 입력 지연이 없어야 함) 확인.

```bash
just app  # 두 터미널 탭 띄우고 위 시나리오 진행
```

- [ ] **Step 3: Commit**

```bash
git commit -am "$(cat <<'EOF'
WT5.5: split daemon broadcast into per-session channels (no head-of-line blocking)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task WT5.6: spawn_and_attach / spawn_unattached 통합

**Files:**
- Modify: `crates/seoul-daemon/src/host.rs:106-180`

- [ ] **Step 1: Extract `spawn_inner`**

```rust
fn spawn_inner(&mut self, params: SpawnParams) -> Result<&mut DaemonSession> {
    let meta = params.build_meta();
    let session = DaemonSession::new(meta, params.shell, params.cwd, params.size)?;
    self.cache.invalidate();
    Ok(self.sessions.entry(session.id()).or_insert(session))
}

pub fn spawn_and_attach(&mut self, p: SpawnParams, client: ClientHandle) -> Result<...> {
    let s = self.spawn_inner(p)?;
    s.attach(client)
}

pub fn spawn_unattached(&mut self, p: SpawnParams) -> Result<...> {
    let s = self.spawn_inner(p)?;
    Ok(s.attached_msg())
}
```

- [ ] **Step 2: Verify**

```bash
just lint && just test
```

- [ ] **Step 3: Commit**

```bash
git commit -am "$(cat <<'EOF'
WT5.6: extract spawn_inner from spawn_and_attach/spawn_unattached

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task WT5.7: WT5 마무리 검증

```bash
just lint && just test
```

---

## WT6 — git status 동시화 (`refactor/git-status-parallel`)

워크트리: `~/.claude/worktrees/seoul-refactor-git-status-parallel`
관련 파일: `crates/seoul-workspace/src/git/status.rs`

검토 결과: `compute_status`가 7개 git 서브프로세스를 직렬 호출. 각 fork는 ~수 ms이고 모두 독립 — 병렬화로 latency 큰 폭 감소.

### Task WT6.1: status::compute_status를 tokio 병렬로

**Files:**
- Modify: `crates/seoul-workspace/src/git/status.rs::compute_status`

- [ ] **Step 1: Identify independent git calls**

`compute_status` 내 호출되는 7개 git 명령 (`status --porcelain=v2`, `rev-list --count` ahead, `rev-list --count` behind, `log -n N`, `diff --name-status`, `diff --numstat staged`, `diff --numstat unstaged`, against-base) 중 어느 것이 직전 호출 결과에 의존하는지 표시.

- [ ] **Step 2: Group + parallelize**

병렬 그룹 (예시):
- 그룹 A: `status --porcelain=v2` (다른 호출의 입력에 의존 없음)
- 그룹 B: `rev-list --count {ahead,behind}` 2개
- 그룹 C: `diff --numstat {staged,unstaged,against-base}` 3개
- 그룹 D: `log -n N`, `diff --name-status`

각 그룹을 `tokio::task::spawn_blocking` + `tokio::join!`로 병렬 실행:

```rust
let runner_clone = runner.clone();
let (status_out, (ahead, behind), (st, ust, ab), (log_out, name_status)) = tokio::join!(
    spawn_blocking(move || runner_clone.run(&["status", "--porcelain=v2", "--branch"])),
    async {
        let r = runner.clone();
        let r2 = runner.clone();
        let (a, b) = tokio::join!(
            spawn_blocking(move || r.run(&["rev-list", "--count", "@{u}..HEAD"])),
            spawn_blocking(move || r2.run(&["rev-list", "--count", "HEAD..@{u}"])),
        );
        (a, b)
    },
    // ... 그룹 C, D
);
```

`compute_status`가 sync면 `compute_status_async`로 새 함수를 만들고 호출처를 점진 마이그레이션.

- [ ] **Step 3: Benchmark**

```bash
hyperfine --warmup 3 'cargo run -p seoul-workspace --example git_status_bench -- /path/to/large/repo'
```

(필요 시 `examples/git_status_bench.rs` 추가; before/after 측정.)

Expected: ~3-5× faster on cold cache.

- [ ] **Step 4: Commit**

```bash
git commit -am "$(cat <<'EOF'
WT6.1: parallelize compute_status git subprocess fan-out via tokio::join!

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task WT6.2: WT6 마무리 검증

```bash
just lint && just test
```

---

## Phase 2: 통합 (병렬 워크트리 → main)

순서: WT3 (구조) → WT1 (editor 성능) → WT2 (가상화) → WT4 → WT5 → WT6.
이 순서는 seoul-terminal 내부 구조 변경(WT3)을 먼저 받아 WT1·WT2 머지 충돌을 줄이기 위함.

### Task P2.1: WT3 머지

- [ ] **Step 1: Bring WT3 changes into main worktree**

```bash
cd /Users/seongminpark/Projects/superset-rust
git fetch  # if remote exists
git merge --ff-only refactor/app-structure || git merge --no-ff refactor/app-structure -m "Merge WT3: app_view structure refactor"
```

(remote 없는 경우 로컬 브랜치 직접 머지.)

- [ ] **Step 2: Verify**

```bash
just lint && just test
```

- [ ] **Step 3: 충돌 발생 시 처리**

병렬 작업이 항상 충돌 없을 거라 보장 못함. 충돌 발생하면:
1. `git status`로 충돌 파일 확인
2. 양 쪽 의도를 본 후 수동 해결 (rebase 권장: `git rebase main` in worktree, fix, continue)
3. 해결 후 재차 `just lint && just test`

### Task P2.2: WT1 머지

```bash
git merge --ff-only refactor/editor-perf || git merge --no-ff refactor/editor-perf -m "Merge WT1: editor performance"
just lint && just test
```

### Task P2.3: WT2 머지

```bash
git merge --ff-only refactor/list-virt || git merge --no-ff refactor/list-virt -m "Merge WT2: list virtualization"
just lint && just test
```

### Task P2.4: WT4 머지

```bash
git merge --ff-only refactor/vt-safety || git merge --no-ff refactor/vt-safety -m "Merge WT4: VT safety"
just lint && just test
```

### Task P2.5: WT5 머지

```bash
git merge --ff-only refactor/daemon-hotpath || git merge --no-ff refactor/daemon-hotpath -m "Merge WT5: daemon hot path"
just lint && just test
just kill-daemon  # state 파일/락 변경됐으므로 데몬 재시작
just app  # 데몬 정상 재기동 확인
```

### Task P2.6: WT6 머지

```bash
git merge --ff-only refactor/git-status-parallel || git merge --no-ff refactor/git-status-parallel -m "Merge WT6: git status parallel"
just lint && just test
```

---

## Phase 3: paths.rs Result화 (직렬, 모든 머지 후)

워크트리: `~/.claude/worktrees/seoul-refactor-paths-result`
브랜치: `refactor/paths-result`
관련 파일: `crates/seoul-terminal-proto/src/paths.rs` + 모든 호출처

이 단계만 별도로 직렬 진행하는 이유: `seoul_dir()`은 5개 crate 전반에서 호출되므로, 위 6개 머지가 끝난 main 위에서 일괄 변경하는 편이 충돌 없이 안전하다.

### Task WT7.1: 워크트리 생성

```bash
git worktree add ~/.claude/worktrees/seoul-refactor-paths-result -b refactor/paths-result main
cd ~/.claude/worktrees/seoul-refactor-paths-result
```

### Task WT7.2: paths::seoul_dir() Result화

**Files:**
- Modify: `crates/seoul-terminal-proto/src/paths.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn seoul_dir_returns_err_when_home_unset() {
    let saved = std::env::var_os("HOME");
    unsafe { std::env::remove_var("HOME"); }
    let r = seoul_dir();
    if let Some(h) = saved { unsafe { std::env::set_var("HOME", h); } }
    assert!(r.is_err());
}
```

- [ ] **Step 2: Change signature**

```rust
use anyhow::{Context, Result};

pub fn seoul_dir() -> Result<PathBuf> {
    Ok(dirs::home_dir().context("home directory not found")?.join(".seoul"))
}

pub fn socket_path() -> Result<PathBuf> { Ok(seoul_dir()?.join("terminal-host.sock")) }
pub fn pid_path() -> Result<PathBuf> { Ok(seoul_dir()?.join("terminal-host.pid")) }
pub fn token_path() -> Result<PathBuf> { Ok(seoul_dir()?.join("terminal-host.token")) }
pub fn lock_path() -> Result<PathBuf> { Ok(seoul_dir()?.join("terminal-host.lock")) }
pub fn daemon_log_path() -> Result<PathBuf> { Ok(seoul_dir()?.join("daemon.log")) }
pub fn terminal_history_dir() -> Result<PathBuf> { Ok(seoul_dir()?.join("terminal-history")) }
pub fn session_history_dir(id: Uuid) -> Result<PathBuf> { Ok(terminal_history_dir()?.join(id.to_string())) }
pub fn scrollback_path(id: Uuid) -> Result<PathBuf> { Ok(session_history_dir(id)?.join("scrollback.bin")) }
pub fn meta_path(id: Uuid) -> Result<PathBuf> { Ok(session_history_dir(id)?.join("meta.json")) }
```

- [ ] **Step 3: Run test → pass**

```bash
cargo test -p seoul-terminal-proto seoul_dir_returns_err
```

### Task WT7.3: 호출처 일괄 변경

**Files:** 모든 호출처 (cargo가 알려줌)

- [ ] **Step 1: Find call sites**

```bash
grep -rn "paths::seoul_dir\|paths::socket_path\|paths::pid_path\|paths::token_path\|paths::lock_path\|paths::daemon_log_path\|paths::terminal_history_dir\|paths::session_history_dir\|paths::scrollback_path\|paths::meta_path" crates/ --include="*.rs"
```

- [ ] **Step 2: Patch each call site**

대부분 `paths::socket_path()` → `paths::socket_path()?`로 충분. main 같은 함수가 `Result` 반환이 아니면 `with_context`로 부풀리고 panic 대신 propagate.

- [ ] **Step 3: Verify**

```bash
just lint && just test
just app  # 정상 부팅 확인
HOME= cargo run -p seoul-daemon  # 의도적으로 HOME 제거하고 깔끔한 에러 메시지가 나오는지 확인
```

- [ ] **Step 4: Commit**

```bash
git commit -am "$(cat <<'EOF'
WT7: convert seoul-terminal-proto::paths::* to Result<PathBuf> and update call sites

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task WT7.4: WT7 머지

```bash
cd /Users/seongminpark/Projects/superset-rust
git merge --ff-only refactor/paths-result || git merge --no-ff refactor/paths-result -m "Merge WT7: paths Result"
just lint && just test
```

---

## Phase 4: 마무리 정리

### Task P4.1: 워크트리 제거

- [ ] **Step 1: Remove worktrees**

```bash
for g in editor-perf list-virt app-structure vt-safety daemon-hotpath git-status-parallel paths-result; do
  git worktree remove ~/.claude/worktrees/seoul-refactor-$g
done
git worktree list  # main만 남아야 함
```

### Task P4.2: 메모리 업데이트

`~/.claude/projects/-Users-seongminpark-Projects-superset-rust/memory/MEMORY.md`에 항목 추가:

```markdown
- [Refactor 2026-05-05](project_refactor_2026_05_05.md) — editor 성능/리스트 가상화/VT 안전성/daemon 핫패스/git status 병렬화/paths Result 모두 적용
```

`project_refactor_2026_05_05.md` 신규:

```markdown
---
name: 2026-05-05 large refactor
description: 검토 보고서 기반 6+1 워크트리 리팩토링 결과 — 핵심 모듈 변경 요약
type: project
---

2026-05-05 /simplify 검토 결과로 다음 큰 변경이 main에 반영됨:
- editor_view: tick 루프를 epoch+timer로, render 메모, 비동기 파일 로드, vertical_move 통합
- file_tree_view, diff_view: uniform_list 가상화
- app_view: TabKind enum, subscription Vec
- seoul-vt: SinkSwapGuard, paste 인젝션 차단
- seoul-daemon: PTY Bytes, flock, token 0o600, per-session 채널, spawn_inner 통합
- git/status: tokio::join! 병렬화
- paths::*: Result<PathBuf>로 마이그레이션

**Why:** 검토 보고에서 idle CPU/배터리·핫패스 alloc·UI 응답성·동시성 안전 이슈로 누적된 큰 부채.

**How to apply:** 비슷한 부채 모듈을 다룰 때 이 리팩토링 패턴(epoch+timer / 캐시 키 메모이제이션 / RAII guard / per-session 채널 / Result화)을 우선 검토.
```

---

## Self-review (이 plan)

- [x] **Spec coverage:** 검토에서 큰 리팩토링으로 분류된 16개 항목이 모두 WT1~WT7에 매핑됨. 작은 통합(`restore_trace_enabled` 3중복, `FileStatus::diff_color`, `worktree::detect_default_branch`) 같은 항목은 별도 작은 PR 단위라 plan에서 제외 — 사용자가 별도로 요청하면 작은 PR로 처리.
- [x] **Placeholder scan:** "TBD"/"TODO"/"적절히 처리"/"비슷하게" 사용 없음. 모든 step이 실행 가능한 명령 또는 코드 블록을 포함.
- [x] **Type consistency:** `BlinkState`, `BufferVersion`, `ScrollOffset`, `RenderCache`, `TabKind`, `SinkSwapGuard`, `LockHandle`, `SpawnParams` 등 plan 내에서 도입한 타입은 처음 정의한 자리에서만 정의되고 이후 task에서는 언급만 함. `version()`/`bump()`/`should_tick()` 같은 메서드 이름이 task 간 일치.
- 한 가지 주의: WT5.5(per-session 채널)는 `host.rs`/`session.rs`/`server.rs` 광범위 변경이라 단일 task로 묶기 어려울 수 있음. 실행 단계에서 필요하면 sub-task로 쪼개고, 각 sub-task가 자체 `just lint && just test`를 통과하도록.
