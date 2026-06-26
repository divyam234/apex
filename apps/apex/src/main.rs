#![forbid(unsafe_code)]

use apex_ui::ApexShell;
use gpui::{AppContext as _, Application, WindowOptions};
use gpui_component::{Root, TitleBar};
use std::env;
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct LaunchOptions {
    workspace: Option<PathBuf>,
    environment: Option<String>,
}

fn parse_launch_options(arguments: impl IntoIterator<Item = std::ffi::OsString>) -> LaunchOptions {
    let mut options = LaunchOptions::default();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        if argument == "--workspace" || argument == "-w" {
            options.workspace = arguments.next().map(PathBuf::from);
        } else if argument == "--environment" || argument == "-e" {
            options.environment = arguments
                .next()
                .map(|value| value.to_string_lossy().into_owned());
        } else if argument != "--"
            && !argument.to_string_lossy().starts_with('-')
            && options.workspace.is_none()
        {
            options.workspace = Some(PathBuf::from(argument));
        }
    }
    options
}

fn launch_options() -> LaunchOptions {
    parse_launch_options(env::args_os().skip(1))
}

fn main() {
    let launch_options = launch_options();
    let app = Application::new().with_assets(gpui_component_assets::Assets);
    app.run(move |cx| {
        gpui_component::init(cx);
        apex_ui::init(cx);
        let launch_options = launch_options.clone();
        cx.spawn(async move |cx| {
            cx.open_window(
                WindowOptions {
                    titlebar: Some(TitleBar::title_bar_options()),
                    ..WindowOptions::default()
                },
                move |window, cx| {
                    let view = cx.new(|cx| {
                        ApexShell::new_with_workspace_and_environment(
                            launch_options.workspace.clone(),
                            launch_options.environment.clone(),
                            window,
                            cx,
                        )
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                },
            )?;
            Ok::<_, anyhow::Error>(())
        })
        .detach();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn parse(arguments: &[&str]) -> LaunchOptions {
        parse_launch_options(arguments.iter().map(OsString::from))
    }

    #[test]
    fn accepts_named_or_positional_workspace_and_environment() {
        assert_eq!(
            parse(&["--workspace", "/tmp/demo", "--environment", "staging"]),
            LaunchOptions {
                workspace: Some(PathBuf::from("/tmp/demo")),
                environment: Some("staging".to_owned()),
            }
        );
        assert_eq!(
            parse(&["-w", "/tmp/demo", "-e", "development"]),
            LaunchOptions {
                workspace: Some(PathBuf::from("/tmp/demo")),
                environment: Some("development".to_owned()),
            }
        );
        assert_eq!(
            parse(&["/tmp/demo"]),
            LaunchOptions {
                workspace: Some(PathBuf::from("/tmp/demo")),
                environment: None,
            }
        );
        assert_eq!(parse(&[]), LaunchOptions::default());
    }
}
