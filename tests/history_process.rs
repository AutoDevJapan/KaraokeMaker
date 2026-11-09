use karaoke_maker::revision::{TimedLine, TimingDraft, load_history, save_history_new};
use std::{path::PathBuf, process::Command};

#[test]
fn history_survives_process_restart_with_undo() {
    if let Some(folder) = std::env::var_os("KARAOKE_HISTORY_PROCESS_TEST") {
        let folder = PathBuf::from(folder);
        if std::env::var_os("KARAOKE_HISTORY_PROCESS_PHASE").as_deref()
            == Some(std::ffi::OsStr::new("save"))
        {
            let mut draft = TimingDraft::from_lrc("[00:01.00]original\n[00:02.00]\n", 5.0).unwrap();
            draft
                .replace(vec![TimedLine {
                    start: 2.0,
                    end: 3.0,
                    text: "changed".into(),
                }])
                .unwrap();
            save_history_new(&folder, &draft).unwrap();
        } else {
            let mut draft = load_history(&folder, 5.0).unwrap().unwrap();
            assert_eq!(draft.lines()[0].text, "changed");
            assert!(draft.undo());
            assert_eq!(draft.lines()[0].text, "original");
            assert!(!draft.undo());
        }
        return;
    }
    let folder = tempfile::tempdir().unwrap();
    for phase in ["save", "restore"] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "history_survives_process_restart_with_undo"])
            .env("KARAOKE_HISTORY_PROCESS_TEST", folder.path())
            .env("KARAOKE_HISTORY_PROCESS_PHASE", phase);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            child.creation_flags(0x08000000);
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "phase {phase}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
