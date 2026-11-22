#[cfg(test)]
mod work_tests {
    use crate::*;
    use crate::cli_launcher::*;
    use crate::terminal::*;
    use crate::updates_uploads::*;
    use crate::web_fetch::*;
    use crate::work_fs::*;
    use std::io::Write;
    use std::time::Duration;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_root(tag: &str) -> std::path::PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("vicuna-work-test-{tag}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `npm run desktop:dev` (debug build) must be distinguishable from the
    /// installed app, and a release build must keep the plain name.
    #[test]
    fn dev_builds_are_titled_vicuna_dev() {
        if cfg!(debug_assertions) {
            assert_eq!(app_title(), "Vicuna (Dev)");
        } else {
            assert_eq!(app_title(), "Vicuna");
        }
    }

    #[test]
    fn validate_rejects_missing_and_relative() {
        assert!(work_canonical_root("/no/such/dir/xyz").is_err());
        assert!(work_canonical_root("relative/path").is_err());
        // filesystem root would crawl TCC-protected spots on macOS
        assert!(work_canonical_root("/").is_err());
        let d = tmp_root("validate");
        assert!(work_canonical_root(d.to_str().unwrap()).is_ok());
    }

    /// The persistent shell must keep cwd and exported env across exec calls
    /// (the whole point of replacing one-shot `sh -c`).
    #[cfg(not(windows))]
    #[tokio::test]
    async fn agent_shell_persists_cwd_and_env() {
        let d = tmp_root("agentshell");
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let root = d.to_string_lossy().to_string();
        let state = AgentShellState::default();
        let r = agent_shell_exec(&state, root.clone(), root.clone(), "pwd".into(), Some(15000)).await.unwrap();
        assert!(!r.running, "pwd should finish: {:?}", r.output);
        assert_eq!(r.exit_code, Some(0), "output: {:?}", r.output);
        assert!(r.output.contains("agentshell"), "got {:?}", r.output);
        // cd persists to the next call
        let r = agent_shell_exec(&state, root.clone(), root.clone(), "cd sub".into(), Some(15000)).await.unwrap();
        assert!(!r.running);
        let r = agent_shell_exec(&state, root.clone(), root.clone(), "pwd".into(), Some(15000)).await.unwrap();
        assert!(r.output.trim().ends_with("sub"), "cwd did not persist: {:?}", r.output);
        // exported env persists
        agent_shell_exec(&state, root.clone(), root.clone(), "export VICUNA_TEST=hello".into(), Some(15000))
            .await
            .unwrap();
        let r = agent_shell_exec(&state, root.clone(), root.clone(), "printf '%s' \"$VICUNA_TEST\"".into(), Some(15000))
            .await
            .unwrap();
        assert!(r.output.contains("hello"), "env did not persist: {:?}", r.output);
        // exit codes are reported (subshell so the session survives)
        let r = agent_shell_exec(&state, root.clone(), root.clone(), "(exit 7)".into(), Some(15000)).await.unwrap();
        assert_eq!(r.exit_code, Some(7), "got {:?}", r.output);
        kill_agent_shells(&state);
    }

    /// A command that outlives the exec wait stays alive: exec_status polls,
    /// exec_kill interrupts, and the shell is reusable afterwards.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn agent_shell_timeout_poll_and_interrupt() {
        let d = tmp_root("agentshell-timeout");
        let root = d.to_string_lossy().to_string();
        let state = AgentShellState::default();
        let r = agent_shell_exec(&state, root.clone(), root.clone(), "sleep 5; echo done".into(), Some(1000))
            .await
            .unwrap();
        assert!(r.running, "expected still-running, got {:?}", r.output);
        let s = agent_shell_status(&state, root.clone(), root.clone()).await.unwrap();
        assert!(s.running, "still sleeping, got {:?}", s.output);
        let r = agent_shell_interrupt(&state, root.clone(), root.clone()).await.unwrap();
        assert!(!r.running, "interrupt should return to prompt");
        let r = agent_shell_exec(&state, root.clone(), root.clone(), "echo after-interrupt".into(), Some(5000))
            .await
            .unwrap();
        assert!(!r.running);
        assert!(r.output.contains("after-interrupt"), "got {:?}", r.output);
        kill_agent_shells(&state);
    }

    /// Interactive stdin: a command blocked on `read` can be answered with
    /// exec_input, then observed via exec_status.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn agent_shell_accepts_stdin() {
        let d = tmp_root("agentshell-stdin");
        let root = d.to_string_lossy().to_string();
        let state = AgentShellState::default();
        let r = agent_shell_exec(&state, root.clone(), root.clone(), "read line; echo \"got:$line\"".into(), Some(1000))
            .await
            .unwrap();
        assert!(r.running, "read should still be waiting, got {:?}", r.output);
        let sent = agent_shell_input(&state, root.clone(), root.clone(), "hello-stdin".into()).await.unwrap();
        assert!(sent.contains("got:hello-stdin"), "input result: {sent:?}");
        // the finished command is reflected by the next status poll
        let s = agent_shell_status(&state, root.clone(), root.clone()).await.unwrap();
        assert!(!s.running, "shell should be idle again, got {:?}", s.output);
        kill_agent_shells(&state);
    }

    /// Shell environment must suppress pagers/editors: a pager would own the
    /// PTY and swallow every later command.
    /// Turn-start attribution: pre-existing user edits are surfaced, and a
    /// later turn reports the delta instead of the model guessing.
    #[tokio::test]
    async fn git_note_reports_existing_dirt_and_turn_delta() {
        let d = tmp_root("gitnote");
        let inited = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&d)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !inited {
            return; // git unavailable on this machine
        }
        let root = d.to_string_lossy().to_string();
        std::fs::write(d.join("user-edit.txt"), "x").unwrap();
        let state = UndoState::default();
        let note = workspace_git_note(&state, "k", &root).await.unwrap();
        assert!(note.contains("BEFORE this turn"), "{note}");
        assert!(note.contains("?? user-edit.txt"), "{note}");
        std::fs::write(d.join("agent-edit.txt"), "y").unwrap();
        let note = workspace_git_note(&state, "k", &root).await.unwrap();
        assert!(note.contains("CHANGED since your previous turn"), "{note}");
        assert!(note.contains("+ ?? agent-edit.txt"), "{note}");
        assert!(note.contains("move scratch to the system temp dir"), "{note}");
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn agent_shell_sets_noninteractive_env() {
        let d = tmp_root("agentshell-env");
        let root = d.to_string_lossy().to_string();
        let state = AgentShellState::default();
        let r = agent_shell_exec(
            &state,
            root.clone(),
            root.clone(),
            "printf '%s|%s|%s|%s|%s' \"$PAGER\" \"$GIT_PAGER\" \"$LESS\" \"$GIT_TERMINAL_PROMPT\" \"$TMPDIR\"".into(),
            Some(15000),
        )
        .await
        .unwrap();
        let out = r.output.trim();
        assert!(out.starts_with("cat|cat|FRX|0|"), "output: {out:?}");
        let tmp = std::env::temp_dir().to_string_lossy().trim_end_matches('/').to_string();
        assert!(
            out.trim_end_matches('/').ends_with(&tmp),
            "TMPDIR should match the file tools' temp root: {out:?} vs {tmp:?}"
        );
        kill_agent_shells(&state);
    }

    /// Multi-line payloads cannot be typed into an interactive PTY; refuse
    /// them before they corrupt anything and point at `write`.
    #[tokio::test]
    async fn agent_shell_rejects_multiline_and_heredocs() {
        let d = tmp_root("agentshell-multiline");
        let root = d.to_string_lossy().to_string();
        let state = AgentShellState::default();
        let err = agent_shell_exec(
            &state,
            root.clone(),
            root.clone(),
            "cat > f.txt <<'EOF'\nhello\nEOF".into(),
            Some(5000),
        )
        .await
        .unwrap_err();
        assert!(err.contains("one command line"), "got {err:?}");
        assert!(err.contains("write tool"), "got {err:?}");
        kill_agent_shells(&state);
    }

    /// The result carries the real cwd and wall-clock duration.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn agent_shell_reports_cwd_and_duration() {
        let d = tmp_root("agentshell-meta");
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let root = d.to_string_lossy().to_string();
        let state = AgentShellState::default();
        agent_shell_exec(&state, root.clone(), root.clone(), "cd sub".into(), Some(15000))
            .await
            .unwrap();
        let r = agent_shell_exec(&state, root.clone(), root.clone(), "pwd".into(), Some(15000))
            .await
            .unwrap();
        let cwd = r.cwd.unwrap_or_default();
        assert!(cwd.ends_with("sub"), "cwd was {cwd:?}");
        assert!(r.duration_ms.is_some());
        kill_agent_shells(&state);
    }

    /// Terminal echo can be re-enabled by a child program; the probe marker
    /// must still parse (previously the echoed command line was mistaken for
    /// the probe output, yielding "exit unknown" + a garbage cwd).
    #[cfg(not(windows))]
    #[tokio::test]
    async fn agent_shell_exit_probe_survives_echo() {
        let d = tmp_root("agentshell-exit-echo");
        let root = d.to_string_lossy().to_string();
        let state = AgentShellState::default();
        agent_shell_exec(&state, root.clone(), root.clone(), "stty echo".into(), Some(15000))
            .await
            .unwrap();
        let ok = agent_shell_exec(&state, root.clone(), root.clone(), "true".into(), Some(15000))
            .await
            .unwrap();
        assert_eq!(ok.exit_code, Some(0), "output={:?} cwd={:?}", ok.output, ok.cwd);
        assert!(
            ok.cwd.as_deref().unwrap_or("").contains("agentshell-exit-echo"),
            "cwd was {:?}",
            ok.cwd
        );
        let bad = agent_shell_exec(&state, root.clone(), root.clone(), "false".into(), Some(15000))
            .await
            .unwrap();
        assert_eq!(bad.exit_code, Some(1), "output={:?} cwd={:?}", bad.output, bad.cwd);
        kill_agent_shells(&state);
    }

    /// A busy shell reports how long it has been stuck (and, when available,
    /// which foreground process owns the TTY).
    #[cfg(not(windows))]
    #[tokio::test]
    async fn agent_shell_busy_error_is_actionable() {
        let d = tmp_root("agentshell-busy");
        let root = d.to_string_lossy().to_string();
        let state = AgentShellState::default();
        let r = agent_shell_exec(&state, root.clone(), root.clone(), "sleep 4".into(), Some(1000))
            .await
            .unwrap();
        assert!(r.running);
        let err = agent_shell_exec(&state, root.clone(), root.clone(), "echo nope".into(), Some(5000))
            .await
            .unwrap_err();
        assert!(err.contains("busy"), "got {err:?}");
        assert!(err.contains("exec_status"), "got {err:?}");
        agent_shell_interrupt(&state, root.clone(), root.clone()).await.unwrap();
        kill_agent_shells(&state);
    }

    /// Ranged reads make large files inspectable instead of blind-write-only.
    #[tokio::test]
    async fn read_offset_limit_pages_long_files() {
        let d = tmp_root("readrange");
        let d_str = d.to_string_lossy().to_string();
        let body: String = (1..=200).map(|i| format!("line {i}\n")).collect();
        std::fs::write(d.join("big.txt"), body).unwrap();
        let r = work_read_file_logic(d_str.clone(), "big.txt".into(), None, Some(50), Some(5))
            .await
            .unwrap();
        assert_eq!(r.content, "line 50\nline 51\nline 52\nline 53\nline 54\n");
        assert!(r.truncated);
        let all = work_read_file_logic(d_str, "big.txt".into(), None, None, None)
            .await
            .unwrap();
        assert!(!all.truncated);
        assert!(all.content.starts_with("line 1\n"));
        assert!(all.content.ends_with("line 200\n"));
    }

    #[test]
    fn join_rejects_escapes() {
        let d = tmp_root("join");
        let canon = work_canonical_root(d.to_str().unwrap()).unwrap();
        assert!(work_join(&canon, "").unwrap() == canon);
        assert!(work_join(&canon, "a/b/../c.txt").unwrap().starts_with(&canon));
        assert!(work_join(&canon, "../../etc/passwd").is_err());
        assert!(work_join(&canon, "/etc/passwd").is_err());
    }

    /// File tools can reach only the system temp dir outside the workspace —
    /// the sanctioned scratch location — and nothing else.
    #[test]
    fn join_allows_temp_but_keeps_the_boundary() {
        let d = tmp_root("join-temp");
        let canon = work_canonical_root(d.to_str().unwrap()).unwrap();
        let scratch = std::env::temp_dir().join("vicuna-join-temp-check.txt");
        let scratch_s = scratch.to_string_lossy().to_string();
        assert!(work_join_allow_temp(&canon, &scratch_s).is_ok(), "{scratch_s}");
        #[cfg(unix)]
        assert!(work_join_allow_temp(&canon, "/tmp/vicuna-join-temp-check.txt").is_ok());
        assert!(work_join(&canon, &scratch_s).is_err());
        assert!(work_join_allow_temp(&canon, "/etc/hosts").is_err());
        assert!(
            work_join_allow_temp(&canon, &format!("{}/../escape", std::env::temp_dir().display()))
                .is_err()
        );
    }

    #[tokio::test]
    async fn scratch_tools_reach_temp_but_not_other_abs_paths() {
        let d = tmp_root("scratch-tools");
        let root = d.to_string_lossy().to_string();
        let scratch = std::env::temp_dir().join(format!(
            "vicuna-scratch-tools-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&scratch).unwrap();
        let spec = scratch.join("spec.json");
        let spec_s = spec.to_string_lossy().to_string();

        work_write_file_logic(root.clone(), spec_s.clone(), "{\"ok\":true}".into())
            .await
            .unwrap();
        let read = work_read_file_logic(root.clone(), spec_s.clone(), None, None, None)
            .await
            .unwrap();
        assert_eq!(read.content, "{\"ok\":true}");
        let listed = work_list_dir_logic(root.clone(), Some(scratch.to_string_lossy().to_string()))
            .await
            .unwrap();
        assert!(listed.iter().any(|e| e.name == "spec.json"));

        assert!(work_write_file_logic(root.clone(), "/etc/hosts".into(), "nope".into())
            .await
            .is_err());
        assert!(work_read_file_logic(root, "/etc/hosts".into(), None, None, None)
            .await
            .is_err());
        std::fs::remove_dir_all(&scratch).ok();
    }

    #[tokio::test]
    async fn fetch_url_rejects_non_http() {
        assert!(work_fetch_url_logic("".into()).await.is_err());
        assert!(work_fetch_url_logic("not a url".into()).await.is_err());
        assert!(work_fetch_url_logic("file:///etc/passwd".into()).await.is_err());
        assert!(work_fetch_url_logic("ftp://example.com/x".into()).await.is_err());
        assert!(work_fetch_url_logic("data:text/plain,hi".into()).await.is_err());
    }

    #[test]
    fn fetch_entity_decode_covers_named_and_numeric() {
        assert_eq!(
            decode_html_entities("a &amp; b &lt;c&gt; &quot;q&quot; &#39;x&#39; &#x27;y&#x27;&nbsp;z"),
            "a & b <c> \"q\" 'x' 'y' z"
        );
        // unknown entities pass through; bare & copies literally
        assert_eq!(decode_html_entities("&foobar; fish & chips"), "&foobar; fish & chips");
    }

    /// KuCoin-style article page: headline in <title>, body in <article>,
    /// Next.js JSON + CSS noise in script/style. The agent must see the
    /// article, not the page chrome or the __NEXT_DATA__ blob.
    #[test]
    fn fetch_html_to_text_keeps_article_drops_chrome() {
        let html = r#"<!doctype html><html><head><title>OpenAI claims to disprove ETH &mdash; KuCoin</title>
<style>.ad{display:none}</style><script>window.__NEXT_DATA__={"props":{"x":1}}</script></head>
<body><nav><a href="/">Home</a><a href="/news">News</a></nav><article><h1>OpenAI claims to disprove the exponential time hypothesis</h1>
<p>ChainCatcher report: OpenAI recently announced that its AI model has <strong>disproven</strong> the hypothesis.</p>
<p>Cost grows exponentially &amp; no efficient algorithm exists &#8212; yet.</p></article>
<!-- a comment --><script>console.log("hi")</script></body></html>"#;
        let text = html_to_text(html);
        assert!(text.contains("OpenAI claims to disprove ETH &mdash; KuCoin"), "title leads, unknown entity kept: {text}");
        assert!(text.contains("OpenAI claims to disprove the exponential time hypothesis"), "{text}");
        assert!(text.contains("ChainCatcher report: OpenAI recently announced that its AI model has disproven the hypothesis."), "{text}");
        assert!(text.contains("Cost grows exponentially & no efficient algorithm exists — yet."), "{text}");
        assert!(!text.contains("__NEXT_DATA__"), "{text}");
        assert!(!text.contains("display:none"), "{text}");
        assert!(!text.contains("console.log"), "{text}");
        assert!(!text.contains("a comment"), "{text}");
        assert!(!text.contains('<'), "{text}");
    }


    #[tokio::test]
    async fn write_read_list_grep_roundtrip() {
        let d = tmp_root("roundtrip");
        let root = d.to_string_lossy().to_string();
        work_write_file(root.clone(), "hello.py".into(), "print('hi')\n".into())
            .await
            .unwrap();
        let entries = work_list_dir(root.clone(), None).await.unwrap();
        assert!(entries.iter().any(|e| e.name == "hello.py" && !e.is_dir));
        let content = work_read_file(root.clone(), "hello.py".into(), None, None, None)
            .await
            .unwrap();
        assert_eq!(content.content, "print('hi')\n");
        assert!(!content.truncated);
        let hits = work_grep(root.clone(), "print".into(), None).await.unwrap();
        assert!(hits.iter().any(|h| h.path == "hello.py" && h.line == 1));
        // outside-root access rejected at every entry point
        assert!(work_read_file(root.clone(), "../outside.txt".into(), None, None, None)
            .await
            .is_err());
    }

    /// The exact flow the agent runs for "write a script, then run it".
    #[tokio::test]
    async fn write_python_then_run_and_capture() {
        let d = tmp_root("fizzbuzz");
        let root = d.to_string_lossy().to_string();
        let script = r#"for i in range(1, 16):
    s = ("Fizz" if i % 3 == 0 else "") + ("Buzz" if i % 5 == 0 else "")
    print(s or i)
"#;
        work_write_file(root.clone(), "fizzbuzz.py".into(), script.into())
            .await
            .unwrap();
        let res = work_exec(root.clone(), "python3 fizzbuzz.py".into(), Some(30000))
            .await
            .unwrap();
        assert!(!res.timed_out);
        assert_eq!(res.exit_code, Some(0));
        let lines: Vec<&str> = res.stdout.lines().collect();
        assert_eq!(lines.len(), 15);
        assert_eq!(lines[2], "Fizz");
        assert_eq!(lines[4], "Buzz");
        assert_eq!(lines[14], "FizzBuzz");
    }

    #[tokio::test]
    async fn exec_echo_and_timeout() {
        let d = tmp_root("exec");
        let root = d.to_string_lossy().to_string();
        let res = work_exec(root.clone(), "echo hello-work".into(), None)
            .await
            .unwrap();
        assert_eq!(res.stdout.trim(), "hello-work");
        assert_eq!(res.exit_code, Some(0));
        let slow = work_exec(root, "sleep 5".into(), Some(1200)).await.unwrap();
        assert!(slow.timed_out);
    }

    #[test]
    fn full_disk_scan_matcher_cases() {
        // refused: recursive walkers rooted at storm-scale roots, including
        // the exact escalation that stormed TCC (`…; find / -name …`)
        for cmd in [
            "find / -name gh",
            "find / -name 'gh*' 2>/dev/null",
            "find /",
            "find -H / -name gh",
            "command -v gh; echo done; find / -name gh",
            "ls /opt/homebrew/bin && find / -name gh",
            "echo $(find / -name gh)",
            "find ~ -name '*.pem'",
            "find ~/ -name x",
            "find ~other -name x",
            "find $HOME -name x",
            "find ${HOME} -name x",
            "find \"/\" -name x",
            "find /Users -name x",
            "find /Users/waiyip -name x",
            "find /System/Volumes/Data -name x",
            "grep -r secret /",
            "grep -R --regexp=foo /",
            "rg --files /",
            "fd passwd /",
            "ls -R /",
            "du -sh /",
            "tree /",
            "sudo find / -name x",
            "FOO=1 find / -name x",
            "{ find / -name x; }",
        ] {
            assert!(
                refuse_full_disk_scan(cmd).is_some(),
                "should refuse: {cmd}"
            );
        }
        // $HOME spelled out is still a whole-home scan; $HOME/sub is bounded
        if let Ok(home) = std::env::var("HOME") {
            assert!(refuse_full_disk_scan(&format!("find {home} -name x")).is_some());
            assert!(refuse_full_disk_scan(&format!("find {home}/proj -name x")).is_none());
        }
        // allowed: narrow scans + everyday commands (incl. the good probes
        // from the same turn that produced the `find /` escalation)
        for cmd in [
            "command -v gh",
            "which gh && gh --version",
            "ls /opt/homebrew/bin/ | grep -i gh",
            "ls /usr/local/bin/ | grep -i gh",
            "/opt/homebrew/bin/gh --version",
            "find . -name '*.rs'",
            "find src -name x",
            "find -name x",
            "find /opt/homebrew -name gh",
            "find /tmp/work -name x",
            "find ~/proj -name x",
            "find $HOME/proj -name x",
            "find /Users/waiyip/Documents -name x",
            "grep -r needle src",
            "grep -e / needle.txt",
            "rg needle /tmp/thing",
            "fd clone src",
            "ls -R src",
            "ls -r /",
            "ls /",
            "du -sh /opt/homebrew",
            "echo /",
            "echo don't find / trouble",
            "python3 fizzbuzz.py",
        ] {
            assert!(
                refuse_full_disk_scan(cmd).is_none(),
                "should allow: {cmd}"
            );
        }
    }

    /// Both exec entry points refuse a full-disk scan with a coaching error
    /// instead of running it into the timeout + a TCC prompt storm.
    #[tokio::test]
    async fn exec_refuses_full_disk_scan() {
        let d = tmp_root("findguard");
        let root = d.to_string_lossy().to_string();
        let err = work_exec(root.clone(), "command -v gh; find / -name gh".into(), None)
            .await
            .unwrap_err();
        assert!(err.contains("whole disk"), "coaching error, got: {err}");
        assert!(err.contains("command -v"), "coaching error, got: {err}");
        // background start refuses the same way (no storm via detached jobs)
        let state = BgExecState::default();
        let err = bg_start(&state, root, "find / -name gh".into())
            .await
            .unwrap_err();
        assert!(err.contains("whole disk"), "coaching error, got: {err}");
        assert!(bg_list(&state).unwrap().is_empty());
    }

    #[tokio::test]
    async fn read_image_roundtrip_and_guardrails() {
        let d = tmp_root("readimage");
        let root = d.to_string_lossy().to_string();
        // 1x1 transparent PNG
        let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        {
            use base64::Engine as _;
            let bytes = base64::engine::general_purpose::STANDARD.decode(png_b64).unwrap();
            std::fs::write(d.join("tiny.png"), &bytes).unwrap();
        }
        std::fs::write(d.join("note.txt"), "hello").unwrap();
        let img = work_read_image(root.clone(), "tiny.png".into()).await.unwrap();
        assert_eq!(img.mime, "image/png");
        assert_eq!(img.base64, png_b64);
        // non-image extension rejected; escapes rejected
        assert!(work_read_image(root.clone(), "note.txt".into()).await.is_err());
        assert!(work_read_image(root.clone(), "../outside.png".into()).await.is_err());
        // text reader now points binaries at view_image
        std::fs::write(d.join("raw.bin"), [0u8, 1, 2, 3]).unwrap();
        let err = work_read_file(root.clone(), "raw.bin".into(), None, None, None).await.unwrap_err();
        assert!(err.contains("view_image"));
    }

    #[test]
    fn show_context_length_picks_arch_window() {
        let info = serde_json::json!({
            "gptoss.context_length": 131072,
            "gptoss.rope.scaling.original_context_length": 4096,
            "general.architecture": "gptoss"
        });
        assert_eq!(parse_show_context_length(&info), Some(131072));
        let empty = serde_json::json!({"general.architecture": "unknown"});
        assert_eq!(parse_show_context_length(&empty), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bg_job_finishes_when_the_shell_does_despite_a_lingering_child() {
        let d = tmp_root("bglinger");
        let state = BgExecState::default();
        // the leftover sleep inherits stdout and keeps it open for 60 s
        let id = bg_start(&state, d.to_string_lossy().to_string(), "sleep 60 & echo started; exit 3".into()).await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        let done = loop {
            let s = bg_snapshot(&state, &id).unwrap();
            if !s.running {
                break s;
            }
            assert!(std::time::Instant::now() < deadline, "job waited for the leftover child");
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert_eq!(done.exit_code, Some(3), "exit status kept");
        assert!(done.stdout.contains("started"));
        // killing a finished job is a no-op (the pid was released) and the
        // reported status stays the real one
        assert_eq!(bg_kill(&state, &id).unwrap().exit_code, Some(3));
    }

    #[tokio::test]
    async fn bg_exec_start_poll_kill_roundtrip() {
        let d = tmp_root("bgexec");
        let root = d.to_string_lossy().to_string();
        let state = BgExecState::default();
        // fast job: completes with captured output
        let id = bg_start(&state, root.clone(), "echo hello-bg".into())
            .await
            .unwrap();
        assert!(id.starts_with("bg-"));
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let done = loop {
            let s = bg_snapshot(&state, &id).unwrap();
            assert_eq!(s.command, "echo hello-bg");
            if !s.running {
                break s;
            }
            assert!(std::time::Instant::now() < deadline, "bg echo never finished");
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert_eq!(done.exit_code, Some(0));
        assert!(done.stdout.contains("hello-bg"));
        // list sees it as finished
        let list = bg_list(&state).unwrap();
        assert!(list.iter().any(|j| j.id == id && !j.running));
        // slow job: starts running, then a kill settles it
        let slow = bg_start(&state, root.clone(), "sleep 30".into())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(bg_snapshot(&state, &slow).unwrap().running);
        bg_kill(&state, &slow).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let s = bg_snapshot(&state, &slow).unwrap();
            if !s.running {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "killed bg job never settled");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // validation + unknown ids rejected
        assert!(bg_start(&state, root.clone(), "   ".into()).await.is_err());
        assert!(bg_snapshot(&state, "bg-999999").is_err());
        assert!(bg_kill(&state, "bg-999999").is_err());
    }

    #[test]
    fn cli_scripts_carry_marker_and_scheme() {
        for script in [CLI_UNIX_SCRIPT, CLI_WINDOWS_SCRIPT] {
            assert!(script.contains(CLI_MARKER), "installer must be recognizable");
            assert!(script.contains("vicuna://open?path="), "must open the app via deep link");
        }
        // unix script defaults to the caller's directory when no arg is given
        assert!(CLI_UNIX_SCRIPT.contains("${1:-$PWD}"));
        assert!(CLI_WINDOWS_SCRIPT.contains("%CD%"));
    }

    #[test]
    fn cli_write_scan_uninstall_roundtrip() {
        let d = tmp_root("cli");
        let dest = cli_write_launcher(&d).unwrap();
        assert_eq!(dest.file_name().unwrap().to_str().unwrap(), cli_exe_name());
        assert!(cli_is_managed(&dest));
        // a foreign binary with the same name is never claimed or removed
        let other = d.join("shadow");
        std::fs::create_dir_all(&other).unwrap();
        let foreign = other.join(cli_exe_name());
        std::fs::write(&foreign, "echo not-vicuna\n").unwrap();
        assert!(!cli_is_managed(&foreign));
        std::fs::remove_file(&dest).unwrap();
        assert!(!dest.exists());
    }

    #[test]
    fn cli_scan_finds_path_entries_in_order() {
        let a = tmp_root("cli-scan-a");
        let b = tmp_root("cli-scan-b");
        std::fs::write(a.join(cli_exe_name()), CLI_MARKER).unwrap();
        std::fs::write(b.join(cli_exe_name()), CLI_MARKER).unwrap();
        let dirs = vec![a.clone(), b.clone()];
        assert_eq!(cli_scan_in(&dirs, cli_exe_name()).unwrap(), a.join(cli_exe_name()));
        assert!(cli_scan_in(&dirs, "no-such-launcher").is_none());
    }

    #[cfg(not(windows))]
    #[test]
    fn work_path_dirs_prioritize_login_path_and_keep_process_extras() {
        let login_bin = std::path::PathBuf::from("/login/bin");
        let shared = std::path::PathBuf::from("/shared/bin");
        let app_only = std::path::PathBuf::from("/app/bin");
        let dirs = work_path_dirs(
            &[login_bin.clone(), shared.clone()],
            &[shared.clone(), app_only.clone()],
        );
        assert_eq!(dirs, vec![login_bin, shared, app_only]);
    }

    #[test]
    fn cli_all_path_dirs_merges_without_dupes() {
        let a = std::path::PathBuf::from("/usr/bin");
        let b = std::path::PathBuf::from("/tmp/fake-home/.local/bin");
        let merged = cli_all_path_dirs(&[a.clone(), b.clone()]);
        // process PATH always contributes; shell extras append once each
        assert!(merged.contains(&b));
        assert_eq!(merged.iter().filter(|d| *d == &a).count(), 1);
        assert_eq!(merged.iter().filter(|d| *d == &b).count(), 1);
    }

    /// The Finder-launch case: the app's own PATH is sparse system dirs, the
    /// launcher lives in a home bin dir only the login shell sees — status
    /// must still find it (no HOME/PATH env mutation: all explicit params).
    #[tokio::test]
    async fn cli_status_finds_launcher_outside_process_path() {
        let home = tmp_root("cli-home");
        let bindir = home.join(".local").join("bin");
        let dest = cli_write_launcher(&bindir).unwrap();
        // effective path as a Finder-launched app would see it: nothing of
        // its own, only the login-shell dir (a bare slice — no process PATH,
        // so the real environment can't leak into this test)
        let all_dirs = vec![bindir.clone()];
        let st = cli_status_inner(&all_dirs, &home).unwrap();
        assert_eq!(st.found, Some(dest.to_string_lossy().to_string()));
        assert!(st.managed);
        assert!(st.on_path);
        assert_eq!(st.install_dir, bindir.to_string_lossy().to_string());
        // uninstall reaches it through the same effective path
        let removed = cli_uninstall_inner(&all_dirs, &home).unwrap();
        assert_eq!(removed, dest.to_string_lossy().to_string());
        assert!(!dest.exists());
    }

    /// Login-shell probe is read-only and must see at least the system dirs.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn cli_shell_path_probe_sees_system_dirs() {
        let dirs = cli_shell_path_dirs().await;
        assert!(
            dirs.iter().any(|d| d == std::path::Path::new("/usr/bin") || d == std::path::Path::new("/bin")),
            "login shell PATH should contain system dirs: {dirs:?}"
        );
    }

    #[tokio::test]
    async fn tree_lists_nested_skips_build_dirs_and_import_roundtrips() {
        let d = tmp_root("tree");
        let root = d.to_string_lossy().to_string();
        work_write_file(root.clone(), "src/main.py".into(), "print(1)\n".into())
            .await
            .unwrap();
        work_write_file(root.clone(), ".hidden-note".into(), "hi\n".into())
            .await
            .unwrap();
        work_write_file(root.clone(), "node_modules/dep/index.js".into(), "x\n".into())
            .await
            .unwrap();
        let tree = work_tree(root.clone(), None).await.unwrap();
        assert!(tree.iter().any(|e| e.path == "src/main.py" && !e.is_dir && e.depth == 1));
        assert!(tree.iter().any(|e| e.path == ".hidden-note"));
        assert!(!tree.iter().any(|e| e.path.starts_with("node_modules")));
        let mut sorted = tree.clone();
        sorted.sort_by(|a, b| a.path.to_lowercase().cmp(&b.path.to_lowercase()));
        assert_eq!(
            tree.iter().map(|e| &e.path).collect::<Vec<_>>(),
            sorted.iter().map(|e| &e.path).collect::<Vec<_>>()
        );
        // import: binary bytes survive base64, escapes rejected
        let raw = vec![0u8, 1, 2, 255, 65, 66];
        let b64 = {
            const ALPH: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut s = String::new();
            for c in raw.chunks(3) {
                let n = (c[0] as u32) << 16
                    | (*c.get(1).unwrap_or(&0) as u32) << 8
                    | (*c.get(2).unwrap_or(&0) as u32);
                s.push(ALPH[((n >> 18) & 63) as usize] as char);
                s.push(ALPH[((n >> 12) & 63) as usize] as char);
                s.push(if c.len() > 1 {
                    ALPH[((n >> 6) & 63) as usize] as char
                } else {
                    '='
                });
                s.push(if c.len() > 2 {
                    ALPH[(n & 63) as usize] as char
                } else {
                    '='
                });
            }
            s
        };
        work_import_file(root.clone(), "assets/blob.bin".into(), b64)
            .await
            .unwrap();
        assert_eq!(std::fs::read(d.join("assets/blob.bin")).unwrap(), raw);
        assert!(work_import_file(root.clone(), "../evil.txt".into(), "aGk=".into())
            .await
            .is_err());
        assert!(decode_base64("!!!").is_err());
    }

    #[tokio::test]
    async fn upload_stage_and_read_roundtrip() {
        let sid = format!("test-session-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
        let _ = std::fs::remove_dir_all(upload_session_dir(&sid).unwrap_or_default());
        // stage
        let abs = upload_stage_file(sid.clone(), "report.txt".into(), "aGVsbG8tdXBsb2Fk".into())
            .await
            .unwrap();
        assert!(abs.contains(&sid));
        assert!(abs.ends_with("report.txt"));
        // read back through the session-scoped reader
        let got = upload_read_file(sid.clone(), "report.txt".into()).await.unwrap();
        assert_eq!(got.content, "hello-upload");
        assert!(!got.truncated);
        // traversal + bad ids rejected (never outside ~/vicuna/uploads/<id>/)
        // traversal names flatten to their final component *inside* the
        // session dir (never outside it); bare "..", "/", "" rejected
        let p = upload_stage_file(sid.clone(), "../../evil.txt".into(), "aGk=".into())
            .await
            .unwrap();
        assert!(p.contains(&sid) && p.ends_with("evil.txt"));
        assert!(upload_read_file(sid.clone(), "evil.txt".into())
            .await
            .is_ok());
        for bad in ["..", "/", ""] {
            assert!(
                upload_stage_file(sid.clone(), bad.into(), "aGk=".into())
                    .await
                    .is_err(),
                "must reject {bad:?}"
            );
        }
        // subdirectories flatten to the bare file name, still contained
        let q = upload_stage_file(sid.clone(), "a/b.txt".into(), "aGk=".into())
            .await
            .unwrap();
        assert!(q.contains(&sid) && q.ends_with("b.txt"));
        assert!(upload_read_file("../other".into(), "report.txt".into())
            .await
            .is_err());
        assert!(upload_read_file(sid.clone(), "missing.txt".into())
            .await
            .is_err());
        // cleanup the temp session dir
        let _ = std::fs::remove_dir_all(upload_session_dir(&sid).unwrap());
    }

    #[test]
    fn open_installer_guard_rejects_outside_paths() {
        // the bridge exposes open_installer to plain browsers: it must never
        // become an arbitrary-file opener
        assert!(open_installer("/etc/hosts").is_err());
        assert!(open_installer("/").is_err());
        assert!(open_installer("").is_err());
        assert!(open_installer("~/Downloads/definitely-missing.dmg").is_err());
        // only installer extensions are allowed, even inside Downloads
        let probe = downloads_dir().unwrap().join("vicuna-guard-test.txt");
        let _ = std::fs::write(&probe, b"x");
        assert!(open_installer(&probe.to_string_lossy()).is_err());
        let _ = std::fs::remove_file(&probe);
    }

    #[test]
    fn release_info_parsing_skips_broken_assets() {        // canned /releases/tags/latest payload (offline test)
        let v = serde_json::json!({
            "tag_name": "latest",
            "published_at": "2026-10-28T18:29:59Z",
            "updated_at": "2026-10-31T15:53:28Z",
            "html_url": "https://github.com/anslwy/vicuna/releases/tag/latest",
            "assets": [
                {
                    "name": "Vicuna_0.1.0_universal.dmg",
                    "browser_download_url": "https://github.com/anslwy/vicuna/releases/download/latest/Vicuna_0.1.0_universal.dmg",
                    "size": 123,
                    "created_at": "2026-10-31T15:53:27Z",
                    "updated_at": "2026-10-31T15:53:28Z"
                },
                {"name": "broken-no-url"},
            ]
        });
        let info = parse_release_info(&v).unwrap();
        assert_eq!(info["tag"], "latest");
        assert_eq!(info["publishedAt"], "2026-10-28T18:29:59Z");
        assert_eq!(info["updatedAt"], "2026-10-31T15:53:28Z");
        let assets = info["assets"].as_array().unwrap();
        assert_eq!(assets.len(), 1);
        // asset timestamps survive — the frontend compares against these,
        // never the frozen release published_at
        assert_eq!(assets[0]["uploadedAt"], "2026-10-31T15:53:28Z");
        // created_at is the fallback when updated_at is missing
        let fallback = parse_release_info(&serde_json::json!({
            "tag_name": "latest",
            "published_at": "2026-10-28T18:29:59Z",
            "assets": [{
                "name": "a.dmg",
                "browser_download_url": "https://example.com/a.dmg",
                "created_at": "2026-10-30T00:00:00Z"
            }]
        }))
        .unwrap();
        assert_eq!(fallback["assets"][0]["uploadedAt"], "2026-10-30T00:00:00Z");
        assert!(parse_release_info(&serde_json::json!({"tag_name": "x"})).is_err());
    }

    #[tokio::test]
    async fn rename_delete_roundtrip_and_guardrails() {
        let d = tmp_root("renamedel");
        let root = d.to_string_lossy().to_string();
        work_write_file(root.clone(), "sub/old.txt".into(), "x".into())
            .await
            .unwrap();
        let p = work_rename(root.clone(), "sub/old.txt".into(), "new.txt".into())
            .await
            .unwrap();
        assert!(p.ends_with("new.txt"));
        assert!(work_read_file(root.clone(), "sub/new.txt".into(), None, None, None)
            .await
            .is_ok());
        assert!(work_read_file(root.clone(), "sub/old.txt".into(), None, None, None)
            .await
            .is_err());
        // rejects escapes, missing sources, slashed/off-root names, clashes
        assert!(work_rename(root.clone(), "../evil.txt".into(), "x".into())
            .await
            .is_err());
        assert!(work_rename(root.clone(), "missing.txt".into(), "x.txt".into())
            .await
            .is_err());
        assert!(work_rename(root.clone(), "sub/new.txt".into(), "a/b.txt".into())
            .await
            .is_err());
        assert!(work_rename(root.clone(), "sub/new.txt".into(), "sub/new.txt".into())
            .await
            .is_err());
        work_write_file(root.clone(), "dir/f.txt".into(), "y".into())
            .await
            .unwrap();
        work_delete(root.clone(), "dir".into()).await.unwrap();
        assert!(work_read_file(root.clone(), "dir/f.txt".into(), None, None, None)
            .await
            .is_err());
        work_write_file(root.clone(), "gone.txt".into(), "y".into())
            .await
            .unwrap();
        work_delete(root.clone(), "gone.txt".into()).await.unwrap();
        assert!(work_delete(root.clone(), ".".into()).await.is_err());
        assert!(work_delete(root.clone(), "../outside".into()).await.is_err());
        assert!(work_delete(root.clone(), "missing.txt".into()).await.is_err());
    }

    #[tokio::test]
    async fn undo_restores_modified_and_removes_created() {
        let d = tmp_root("undo");
        let root = d.to_string_lossy().to_string();
        std::fs::write(d.join("keep.txt"), b"before").unwrap();
        let state = UndoState::default();
        let key = "sess-undo-test";
        // turn 1: modify keep.txt + create new.txt
        undo_begin_turn(&state, key, &root);
        undo_record(&state, key, UndoOp::Modified { path: "keep.txt".into(), prev: b"before".to_vec(), next: b"after".to_vec() });
        work_write_file(root.clone(), "keep.txt".into(), "after".into()).await.unwrap();
        undo_record(&state, key, UndoOp::Created { path: "new.txt".into(), content: b"new".to_vec() });
        work_write_file(root.clone(), "new.txt".into(), "new".into()).await.unwrap();
        // turn 2 (empty): only an exec note
        undo_begin_turn(&state, key, &root);
        undo_record(&state, key, UndoOp::ExecNote { command: "echo hi".into() });
        let msg = work_undo_logic(&state, key.to_string()).await.unwrap();
        assert!(msg.contains("no file writes"), "got {msg:?}");
        assert_eq!(std::fs::read_to_string(d.join("keep.txt")).unwrap(), "after");
        let msg = work_undo_logic(&state, key.to_string()).await.unwrap();
        assert!(msg.contains("restored 1 file") && msg.contains("removed 1 created file"), "got {msg:?}");
        assert_eq!(std::fs::read_to_string(d.join("keep.txt")).unwrap(), "before");
        assert!(!d.join("new.txt").exists());
        // /redo puts the turn's bytes back, then /undo can revert it again
        let msg = work_redo_logic(&state, key.to_string()).await.unwrap();
        assert!(msg.contains("restored 1 file") && msg.contains("recreated 1 file"), "got {msg:?}");
        assert_eq!(std::fs::read_to_string(d.join("keep.txt")).unwrap(), "after");
        assert_eq!(std::fs::read_to_string(d.join("new.txt")).unwrap(), "new");
        let msg = work_undo_logic(&state, key.to_string()).await.unwrap();
        assert!(msg.contains("restored 1 file"), "got {msg:?}");
        assert_eq!(std::fs::read_to_string(d.join("keep.txt")).unwrap(), "before");
        // a fresh turn drops the redo history
        let msg = work_redo_logic(&state, "other-session".to_string()).await.unwrap_err();
        assert!(msg.contains("Nothing to redo"), "got {msg:?}");
        undo_begin_turn(&state, key, &root);
        assert!(work_redo_logic(&state, key.to_string()).await.is_err());
        assert_eq!(work_undo_logic(&state, key.to_string()).await.unwrap(), "Undid last turn: no file changes to revert.");
        assert!(work_undo_logic(&state, key.to_string()).await.is_err());
    }

    /// Real pty roundtrip: spawn a login shell, run echo, read it back,
    /// then kill and observe the exit event.
    #[test]
    fn pty_echo_roundtrip_and_kill() {
        let d = tmp_root("pty");
        let root = d.to_string_lossy().to_string();
        let out = std::sync::Arc::new(Mutex::new(String::new()));
        let out2 = out.clone();
        let (tx, rx) = std::sync::mpsc::channel::<Option<i32>>();
        let h = spawn_pty(
            &root,
            Some(80),
            Some(24),
            move |t| {
                out2.lock().unwrap().push_str(&t);
            },
            move |c| {
                let _ = tx.send(c);
            },
        )
        .unwrap();
        // login shells need a moment; then a marker roundtrips through echo
        std::thread::sleep(Duration::from_secs(3));
        {
            let mut w = h.writer.lock().unwrap();
            w.write_all(b"echo pty-marker-84135\n").unwrap();
            w.flush().unwrap();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            if out.lock().unwrap().contains("pty-marker-84135") {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "no echo from pty shell");
            std::thread::sleep(Duration::from_millis(200));
        }
        // kill by pid (same path term_kill uses) and expect the exit event
        kill_pid(h.pid.expect("shell has a pid"));
        let code = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("no exit event after kill");
        // SIGKILLed shells report no status code on unix
        #[cfg(not(windows))]
        assert_eq!(code, None);
        #[cfg(windows)]
        let _ = code;
    }
}

#[cfg(test)]
mod fs_list_dirs_tests {
    use crate::fs_list_dirs_logic;

    #[test]
    fn lists_only_folders_sorted_and_hides_dotdirs() {
        let root = std::env::temp_dir().join(format!("vicuna-fsl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["beta", "Alpha", ".hidden", "gamma/inner"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("file.txt"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("beta"), root.join("link-to-beta")).unwrap();

        let v = fs_list_dirs_logic(Some(root.to_string_lossy().into()), false).unwrap();
        let names: Vec<&str> = v["entries"].as_array().unwrap().iter().map(|e| e.as_str().unwrap()).collect();
        #[cfg(unix)]
        assert_eq!(names, ["Alpha", "beta", "gamma", "link-to-beta"]);
        assert!(v["parent"].is_string());
        let with_hidden = fs_list_dirs_logic(Some(root.to_string_lossy().into()), true).unwrap();
        assert!(with_hidden["entries"].as_array().unwrap().iter().any(|e| e == ".hidden"));

        assert!(fs_list_dirs_logic(Some("relative/path".into()), false).is_err());
        assert!(fs_list_dirs_logic(Some(root.join("file.txt").to_string_lossy().into()), false).is_err());
        assert!(fs_list_dirs_logic(Some(root.join("missing").to_string_lossy().into()), false).is_err());
        // empty = home
        let home = fs_list_dirs_logic(None, false).unwrap();
        assert_eq!(home["path"], home["home"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
