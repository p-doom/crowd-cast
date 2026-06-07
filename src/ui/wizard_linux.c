// Native GTK3 setup wizard for Linux.
//
// Mirrors the macOS Cocoa wizard (src/ui/wizard_darwin.m) and exposes the same
// C ABI consumed by src/installer/wizard_ffi.rs (WizardAppInfo / WizardConfig).
// The system tray is a separate concern (still disabled via no_tray); this file
// implements only the first-run setup wizard.
#include <gtk/gtk.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

// Must match #[repr(C)] WizardAppInfo in wizard_ffi.rs
typedef struct {
    const char *bundle_id;
    const char *name;
    uint32_t pid;
} WizardAppInfo;

// Must match #[repr(C)] WizardConfig in wizard_ffi.rs
typedef struct {
    bool capture_all;
    bool enable_autostart;
    const char **selected_apps;
    size_t selected_apps_count;
    bool completed;
    bool cancelled;
} WizardConfig;

// Owned copy of the available apps, set via wizard_set_apps.
static char **g_app_ids = NULL;
static char **g_app_names = NULL;
static size_t g_app_count = 0;

static void free_stored_apps(void) {
    if (g_app_ids && g_app_names) {
        for (size_t i = 0; i < g_app_count; i++) {
            free(g_app_ids[i]);
            free(g_app_names[i]);
        }
    }
    free(g_app_ids);
    free(g_app_names);
    g_app_ids = NULL;
    g_app_names = NULL;
    g_app_count = 0;
}

void wizard_set_apps(const WizardAppInfo *apps, size_t count) {
    free_stored_apps();
    if (!apps || count == 0) return;
    g_app_ids = (char **)calloc(count, sizeof(char *));
    g_app_names = (char **)calloc(count, sizeof(char *));
    if (!g_app_ids || !g_app_names) { free_stored_apps(); return; }
    for (size_t i = 0; i < count; i++) {
        g_app_ids[i] = strdup(apps[i].bundle_id ? apps[i].bundle_id : "");
        g_app_names[i] = strdup(apps[i].name ? apps[i].name : "");
    }
    g_app_count = count;
}

int wizard_run(WizardConfig *out) {
    if (!out) return 1;
    out->capture_all = false;
    out->enable_autostart = false;
    out->selected_apps = NULL;
    out->selected_apps_count = 0;
    out->completed = false;
    out->cancelled = true;

    // gtk_init_check returns FALSE (instead of aborting) if there is no display.
    if (!gtk_init_check(NULL, NULL)) {
        return 2;
    }

    GtkWidget *dialog = gtk_dialog_new_with_buttons(
        "crowd-cast setup", NULL, GTK_DIALOG_MODAL,
        "_Cancel", GTK_RESPONSE_CANCEL,
        "_Finish", GTK_RESPONSE_OK,
        NULL);
    gtk_window_set_default_size(GTK_WINDOW(dialog), 480, 440);

    GtkWidget *content = gtk_dialog_get_content_area(GTK_DIALOG(dialog));
    gtk_container_set_border_width(GTK_CONTAINER(content), 16);
    gtk_box_set_spacing(GTK_BOX(content), 10);

    GtkWidget *title = gtk_label_new(NULL);
    gtk_label_set_markup(GTK_LABEL(title),
        "<span size='x-large' weight='bold'>Welcome to crowd-cast</span>");
    gtk_label_set_xalign(GTK_LABEL(title), 0.0);
    gtk_box_pack_start(GTK_BOX(content), title, FALSE, FALSE, 0);

    GtkWidget *intro = gtk_label_new(
        "crowd-cast records your screen and input to build training data.\n"
        "Choose what to capture, then click Finish.");
    gtk_label_set_xalign(GTK_LABEL(intro), 0.0);
    gtk_label_set_line_wrap(GTK_LABEL(intro), TRUE);
    gtk_box_pack_start(GTK_BOX(content), intro, FALSE, FALSE, 0);

    GtkWidget *perm = gtk_label_new(NULL);
    gtk_label_set_markup(GTK_LABEL(perm),
        "<i>Input capture on Wayland requires your user to be in the 'input' group:\n"
        "sudo usermod -aG input $USER  \xE2\x80\x94  then log out and back in.</i>");
    gtk_label_set_xalign(GTK_LABEL(perm), 0.0);
    gtk_label_set_line_wrap(GTK_LABEL(perm), TRUE);
    gtk_box_pack_start(GTK_BOX(content), perm, FALSE, FALSE, 0);

    GtkWidget *capture_all = gtk_check_button_new_with_label(
        "Capture all applications (recommended)");
    gtk_toggle_button_set_active(GTK_TOGGLE_BUTTON(capture_all), TRUE);
    gtk_box_pack_start(GTK_BOX(content), capture_all, FALSE, FALSE, 0);

    GtkWidget *apps_label = gtk_label_new("Or select specific applications:");
    gtk_label_set_xalign(GTK_LABEL(apps_label), 0.0);
    gtk_box_pack_start(GTK_BOX(content), apps_label, FALSE, FALSE, 0);

    GtkWidget *scroll = gtk_scrolled_window_new(NULL, NULL);
    gtk_scrolled_window_set_policy(GTK_SCROLLED_WINDOW(scroll),
        GTK_POLICY_NEVER, GTK_POLICY_AUTOMATIC);
    gtk_widget_set_vexpand(scroll, TRUE);
    GtkWidget *apps_box = gtk_box_new(GTK_ORIENTATION_VERTICAL, 4);
    gtk_container_add(GTK_CONTAINER(scroll), apps_box);
    gtk_box_pack_start(GTK_BOX(content), scroll, TRUE, TRUE, 0);

    GtkWidget **app_checks = NULL;
    if (g_app_count > 0) {
        app_checks = (GtkWidget **)calloc(g_app_count, sizeof(GtkWidget *));
        for (size_t i = 0; i < g_app_count; i++) {
            const char *label =
                (g_app_names[i] && g_app_names[i][0]) ? g_app_names[i] : g_app_ids[i];
            app_checks[i] = gtk_check_button_new_with_label(label);
            gtk_box_pack_start(GTK_BOX(apps_box), app_checks[i], FALSE, FALSE, 0);
        }
    } else {
        GtkWidget *none = gtk_label_new("(no capturable applications detected)");
        gtk_label_set_xalign(GTK_LABEL(none), 0.0);
        gtk_box_pack_start(GTK_BOX(apps_box), none, FALSE, FALSE, 0);
    }

    GtkWidget *autostart = gtk_check_button_new_with_label("Start crowd-cast on login");
    gtk_box_pack_start(GTK_BOX(content), autostart, FALSE, FALSE, 0);

    gtk_widget_show_all(dialog);
    gint resp = gtk_dialog_run(GTK_DIALOG(dialog));

    if (resp == GTK_RESPONSE_OK) {
        out->completed = true;
        out->cancelled = false;
        out->capture_all = gtk_toggle_button_get_active(GTK_TOGGLE_BUTTON(capture_all));
        out->enable_autostart = gtk_toggle_button_get_active(GTK_TOGGLE_BUTTON(autostart));

        if (!out->capture_all && g_app_count > 0 && app_checks) {
            size_t n = 0;
            for (size_t i = 0; i < g_app_count; i++) {
                if (gtk_toggle_button_get_active(GTK_TOGGLE_BUTTON(app_checks[i]))) n++;
            }
            if (n > 0) {
                const char **sel = (const char **)calloc(n, sizeof(char *));
                size_t j = 0;
                for (size_t i = 0; i < g_app_count && j < n; i++) {
                    if (gtk_toggle_button_get_active(GTK_TOGGLE_BUTTON(app_checks[i]))) {
                        sel[j++] = strdup(g_app_ids[i]);
                    }
                }
                out->selected_apps = sel;
                out->selected_apps_count = n;
            }
        }
    }

    free(app_checks);
    gtk_widget_destroy(dialog);
    while (gtk_events_pending()) gtk_main_iteration();
    return 0;
}

void wizard_free_result(WizardConfig *cfg) {
    if (!cfg || !cfg->selected_apps) return;
    for (size_t i = 0; i < cfg->selected_apps_count; i++) {
        free((void *)cfg->selected_apps[i]);
    }
    free((void *)cfg->selected_apps);
    cfg->selected_apps = NULL;
    cfg->selected_apps_count = 0;
}
