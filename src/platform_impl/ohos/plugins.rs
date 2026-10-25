//! Typed platform capabilities, registered while the Ability initializer is running.
//!
//! The application must install the matching ArkTS HARs in `NativeAbility.bridgePlugins`.
//! `extended-plugins` adds menus, notifications, process control, system and accessibility state.

use openharmony_ability::OpenHarmonyApp;

pub use openharmony_ability_plugin_window as window;

pub use openharmony_ability_plugin_app_control as app_control;
pub use openharmony_ability_plugin_clipboard as clipboard;
pub use openharmony_ability_plugin_files as files;
pub use openharmony_ability_plugin_permission as permission;
pub use openharmony_ability_plugin_url as url;

#[cfg(feature = "extended-plugins")]
pub use openharmony_ability_plugin_accessibility as accessibility;
#[cfg(feature = "extended-plugins")]
pub use openharmony_ability_plugin_menu as menu;
#[cfg(feature = "extended-plugins")]
pub use openharmony_ability_plugin_notification as notification;
#[cfg(feature = "extended-plugins")]
pub use openharmony_ability_plugin_process as process;
#[cfg(feature = "extended-plugins")]
pub use openharmony_ability_plugin_system_state as system_state;

pub(crate) fn register_enabled(app: &OpenHarmonyApp) -> openharmony_ability::napi_ohos::Result<()> {
    if app
        .registered_plugin::<window::WindowBridgePlugin>()?
        .is_none()
    {
        app.register_plugin(window::WindowBridgePlugin)?;
    }

    macro_rules! register {
        ($plugin:expr, $ty:ty) => {
            if app.registered_plugin::<$ty>()?.is_none() {
                app.register_plugin($plugin)?;
            }
        };
    }
    register!(
        clipboard::ClipboardBridgePlugin::default(),
        clipboard::ClipboardBridgePlugin
    );
    register!(url::UrlBridgePlugin, url::UrlBridgePlugin);
    register!(files::FilesBridgePlugin, files::FilesBridgePlugin);
    register!(
        app_control::AppControlBridgePlugin,
        app_control::AppControlBridgePlugin
    );
    register!(
        permission::PermissionBridgePlugin,
        permission::PermissionBridgePlugin
    );
    #[cfg(feature = "extended-plugins")]
    {
        register!(menu::MenuBridgePlugin, menu::MenuBridgePlugin);
        register!(
            notification::NotificationBridgePlugin,
            notification::NotificationBridgePlugin
        );
        register!(process::ProcessBridgePlugin, process::ProcessBridgePlugin);
        register!(
            system_state::SystemStateBridgePlugin,
            system_state::SystemStateBridgePlugin
        );
        register!(
            accessibility::AccessibilityBridgePlugin::new(app),
            accessibility::AccessibilityBridgePlugin
        );
        super::events::register_plugins(app);
    }
    Ok(())
}

/// Menu plugin keys use `main` for the root and decimal IDs for child windows.
#[cfg(feature = "extended-plugins")]
pub fn menu_window_key(id: crate::window::WindowId) -> String {
    let raw = u64::from(id);
    if raw == 0 {
        "main".into()
    } else {
        raw.to_string()
    }
}
