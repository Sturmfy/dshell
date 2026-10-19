//! Window-owned native pickers shared by dshell's pages and the content bridge.
//!
//! The owner HWND is the whole point: an unowned dialog is not modal to dshell
//! and can open behind the main window, which on a maximized window looks like
//! a button that did nothing. DSH's Host chooser has exactly that problem; this
//! module is the shell-owned replacement used by `__DSH_DIRECTORY_PICKER__`.
//!
//! One picker at a time. A second request gets a readable busy error instead of
//! an invisible queue, because neither surface shows a waiting indicator while
//! a native dialog is open.

use std::sync::Arc;

use tauri::{AppHandle, Manager};

use crate::error::{Failure, FailureCode};
use crate::state::AppState;

/// The main window's HWND, so a native dialog can own itself to it.
///
/// Without an owner a dialog is not modal to dshell and may open *behind* the
/// main window — on a maximized window that is indistinguishable from "the button
/// did nothing". The handle travels as an integer because that is all the
/// hand-declared Win32 surface needs.
pub(crate) fn dialog_owner(app: &AppHandle) -> Option<isize> {
    #[cfg(windows)]
    {
        let window = app.get_window(crate::MAIN_WINDOW)?;
        window.hwnd().ok().map(|hwnd| hwnd.0 as isize)
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        None
    }
}

fn state_of(app: &AppHandle) -> Result<Arc<AppState>, Failure> {
    app.try_state::<Arc<AppState>>()
        .map(|state| state.inner().clone())
        .ok_or_else(|| Failure::new(FailureCode::Internal, "应用状态尚未就绪"))
}

/// Open one owned folder picker.
///
/// `title` is the dialog caption. The caller chooses it because the workspace
/// flow and dshell's settings both use this picker for different questions.
pub async fn pick_directory(
    app: &AppHandle,
    initial: Option<String>,
    title: &str,
) -> Result<Option<String>, Failure> {
    let state = state_of(app)?;
    let _guard = state.picker_lock.try_lock().map_err(|_| {
        Failure::new(
            FailureCode::Internal,
            "另一个文件选择器已打开，请先完成或取消它",
        )
    })?;
    let owner = dialog_owner(app);
    let title = title.to_string();
    let initial_display = initial.clone();
    let result = tokio::task::spawn_blocking(move || {
        crate::dialog::pick_directory(initial.as_deref(), owner, &title)
    })
    .await
    .map_err(|err| Failure::new(FailureCode::Internal, format!("目录选择器任务失败：{err}")))?
    .map_err(|err| Failure::new(FailureCode::Internal, format!("目录选择器失败：{err}")))?;
    crate::dsh::process::debug_log(
        &state.data_root.path,
        &format!(
            "pick_directory(initial={initial_display:?}) -> {}",
            match &result {
                Some(path) => path.to_string(),
                None => "cancelled".to_string(),
            }
        ),
    );
    Ok(result)
}

/// Open one owned file picker with the same serialisation as the folder picker.
pub async fn pick_file(
    app: &AppHandle,
    title: Option<String>,
) -> Result<Option<String>, Failure> {
    let state = state_of(app)?;
    let _guard = state.picker_lock.try_lock().map_err(|_| {
        Failure::new(
            FailureCode::Internal,
            "另一个文件选择器已打开，请先完成或取消它",
        )
    })?;
    let owner = dialog_owner(app);
    let title = title.unwrap_or_else(|| "选择 DSH 后端可执行文件".to_string());
    let result = tokio::task::spawn_blocking(move || {
        crate::dialog::pick_file(Some(&title), owner)
    })
    .await
    .map_err(|err| Failure::new(FailureCode::Internal, format!("文件选择器任务失败：{err}")))?
    .map_err(|err| Failure::new(FailureCode::Internal, format!("文件选择器失败：{err}")))?;
    crate::dsh::process::debug_log(
        &state.data_root.path,
        &format!(
            "pick_file() -> {}",
            match &result {
                Some(path) => path.to_string(),
                None => "cancelled".to_string(),
            }
        ),
    );
    Ok(result)
}
