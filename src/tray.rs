use crate::state::SharedState;
use ksni::menu::{StandardItem, SubMenu};
use ksni::{Icon, MenuItem, ToolTip, Tray};

pub struct RecorderTray {
    pub state: SharedState,
}

impl Tray for RecorderTray {
    fn id(&self) -> String {
        "meeting-recorder".into()
    }

    fn title(&self) -> String {
        "Meeting Recorder".into()
    }

    fn icon_name(&self) -> String {
        let recording = self.state.lock().unwrap().recording;
        if recording {
            "media-record".into()
        } else {
            "audio-input-microphone".into()
        }
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        Vec::new() // rely on icon_name / theme icons; keeps this dependency-light
    }

    fn tool_tip(&self) -> ToolTip {
        let st = self.state.lock().unwrap();
        let desc = if st.recording {
            format!("Recording ({})", st.current_platform.clone().unwrap_or_else(|| "unknown".into()))
        } else {
            "Idle - watching for meetings".to_string()
        };
        ToolTip {
            title: "Meeting Recorder".into(),
            description: desc,
            icon_name: String::new(),
            icon_pixmap: Vec::new(),
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let st = self.state.lock().unwrap();
        let status_label = if st.recording {
            format!(
                "\u{25CF} Recording \u{2014} {}",
                st.current_platform.clone().unwrap_or_else(|| "unknown platform".into())
            )
        } else {
            "\u{25CB} Idle \u{2014} watching for meetings".to_string()
        };
        let (today_count, today_secs) = st.stats.today_summary();
        let today_label = format!(
            "Today: {} recording(s), {}m {}s",
            today_count,
            today_secs / 60,
            today_secs % 60
        );
        let force_label = if st.recording { "Force stop recording" } else { "Force start recording" };
        drop(st);

        vec![
            StandardItem { label: status_label, enabled: false, ..Default::default() }.into(),
            StandardItem { label: today_label, enabled: false, ..Default::default() }.into(),
            MenuItem::Separator,
            StandardItem {
                label: force_label.to_string(),
                activate: Box::new(|tray: &mut RecorderTray| {
                    let mut st = tray.state.lock().unwrap();
                    st.toggle_force_override();
                }),
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: "Settings".into(),
                submenu: vec![
                    StandardItem {
                        label: "Open config file location".into(),
                        activate: Box::new(|_: &mut RecorderTray| {
                            if let Some(dir) = dirs::config_dir() {
                                let _ = std::process::Command::new("xdg-open")
                                    .arg(dir.join("meeting-recorder"))
                                    .spawn();
                            }
                        }),
                        ..Default::default()
                    }
                    .into(),
                    StandardItem {
                        label: "Open recordings folder".into(),
                        activate: Box::new(|tray: &mut RecorderTray| {
                            let dir = tray.state.lock().unwrap().output_dir.clone();
                            let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
                        }),
                        ..Default::default()
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|_: &mut RecorderTray| {
                    std::process::exit(0);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}
