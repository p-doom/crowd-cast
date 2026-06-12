// Native GTK3 setup wizard for Linux.
//
// Mirrors the macOS Cocoa wizard (src/ui/wizard_darwin.m) and exposes the same
// C ABI consumed by src/installer/wizard_ffi.rs (WizardAppInfo / WizardConfig),
// plus a Linux-only WizardRequirement list used to render a system-requirements
// checklist and gate the Finish button (the analog of macOS permission gating).
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

// Must match #[repr(C)] WizardRequirement in wizard_ffi.rs
// severity: 0 = Required, 1 = Recommended, 2 = Optional
typedef struct {
    const char *label;
    const char *detail;
    const char *command;
    uint32_t severity;
    bool satisfied;
} WizardRequirement;

// Must match #[repr(C)] FfiAppSelectionResult in src/ui/app_selector.rs.
// Result of the tray "Settings" dialog (show_app_selection_panel below).
typedef struct {
    bool capture_all;
    const char **selected_apps;
    size_t selected_apps_count;
    bool saved;
} AppSelectionResult;

// ---- available apps (owned copy) ----
static char **g_app_ids = NULL;
static char **g_app_names = NULL;
static size_t g_app_count = 0;

// ---- host requirements (owned copy) ----
static char **g_req_labels = NULL;
static char **g_req_details = NULL;
static char **g_req_cmds = NULL;
static uint32_t *g_req_sev = NULL;
static bool *g_req_sat = NULL;
static size_t g_req_count = 0;

// Whether per-app (per-window) capture is available; when false the wizard greys out
// the app picker and forces full-screen capture (set via FFI).
static gboolean g_per_app_available = TRUE;

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

static void free_stored_reqs(void) {
    if (g_req_labels && g_req_details && g_req_cmds) {
        for (size_t i = 0; i < g_req_count; i++) {
            free(g_req_labels[i]);
            free(g_req_details[i]);
            free(g_req_cmds[i]);
        }
    }
    free(g_req_labels);
    free(g_req_details);
    free(g_req_cmds);
    free(g_req_sev);
    free(g_req_sat);
    g_req_labels = NULL;
    g_req_details = NULL;
    g_req_cmds = NULL;
    g_req_sev = NULL;
    g_req_sat = NULL;
    g_req_count = 0;
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

void wizard_set_requirements(const WizardRequirement *reqs, size_t count) {
    free_stored_reqs();
    if (!reqs || count == 0) return;
    g_req_labels = (char **)calloc(count, sizeof(char *));
    g_req_details = (char **)calloc(count, sizeof(char *));
    g_req_cmds = (char **)calloc(count, sizeof(char *));
    g_req_sev = (uint32_t *)calloc(count, sizeof(uint32_t));
    g_req_sat = (bool *)calloc(count, sizeof(bool));
    if (!g_req_labels || !g_req_details || !g_req_cmds || !g_req_sev || !g_req_sat) {
        free_stored_reqs();
        return;
    }
    for (size_t i = 0; i < count; i++) {
        g_req_labels[i] = strdup(reqs[i].label ? reqs[i].label : "");
        g_req_details[i] = strdup(reqs[i].detail ? reqs[i].detail : "");
        g_req_cmds[i] = strdup(reqs[i].command ? reqs[i].command : "");
        g_req_sev[i] = reqs[i].severity;
        g_req_sat[i] = reqs[i].satisfied;
    }
    g_req_count = count;
}

// Build one requirement row: a colored marker + label, with the fix detail
// underneath when the requirement is unmet.
static GtkWidget *make_req_row(const char *label, const char *detail,
                               const char *command, uint32_t severity, bool satisfied) {
    GtkWidget *row = gtk_box_new(GTK_ORIENTATION_HORIZONTAL, 8);

    const char *marker;
    if (satisfied) {
        marker = "<span foreground='#2e7d32' weight='bold'>OK</span>"; // green check
    } else if (severity == 0) {
        marker = "<span foreground='#c62828' weight='bold'>X</span>"; // red x
    } else if (severity == 1) {
        marker = "<span foreground='#f9a825' weight='bold'>!</span>"; // amber warn
    } else {
        marker = "<span foreground='#888888'>-</span>"; // grey circle
    }
    GtkWidget *m = gtk_label_new(NULL);
    gtk_label_set_markup(GTK_LABEL(m), marker);
    gtk_widget_set_valign(m, GTK_ALIGN_START);
    gtk_box_pack_start(GTK_BOX(row), m, FALSE, FALSE, 0);

    GtkWidget *col = gtk_box_new(GTK_ORIENTATION_VERTICAL, 1);
    GtkWidget *lbl = gtk_label_new(label);
    gtk_label_set_xalign(GTK_LABEL(lbl), 0.0);
    gtk_label_set_line_wrap(GTK_LABEL(lbl), TRUE);
    gtk_box_pack_start(GTK_BOX(col), lbl, FALSE, FALSE, 0);
    if (!satisfied && detail && detail[0]) {
        GtkWidget *d = gtk_label_new(NULL);
        char *esc = g_markup_escape_text(detail, -1);
        char *markup = g_strdup_printf("<small><i>%s</i></small>", esc);
        gtk_label_set_markup(GTK_LABEL(d), markup);
        g_free(markup);
        g_free(esc);
        gtk_label_set_xalign(GTK_LABEL(d), 0.0);
        gtk_label_set_line_wrap(GTK_LABEL(d), TRUE);
        gtk_box_pack_start(GTK_BOX(col), d, FALSE, FALSE, 0);
    }
    if (!satisfied && command && command[0]) {
        GtkWidget *c = gtk_label_new(NULL);
        char *esc = g_markup_escape_text(command, -1);
        char *markup = g_strdup_printf("<tt>%s</tt>", esc);
        gtk_label_set_markup(GTK_LABEL(c), markup);
        g_free(markup);
        g_free(esc);
        gtk_label_set_xalign(GTK_LABEL(c), 0.0);
        gtk_label_set_selectable(GTK_LABEL(c), TRUE);
        gtk_label_set_line_wrap(GTK_LABEL(c), TRUE);
        gtk_widget_set_tooltip_text(c, "Select this line and copy it (Ctrl+C)");
        gtk_box_pack_start(GTK_BOX(col), c, FALSE, FALSE, 0);
    }
    gtk_box_pack_start(GTK_BOX(row), col, TRUE, TRUE, 0);
    return row;
}

void wizard_set_per_app_available(bool available) {
    g_per_app_available = available ? TRUE : FALSE;
}

// ---- Shared capture-mode UI: "Capture all" + a checklist of apps ----------------
// Used by BOTH the first-run wizard and the tray "Settings" dialog. The user ticks the
// apps to capture, mirroring the macOS panel. Each ticked app gates input recording and
// is captured per-app: by window identity on GNOME Wayland (Mutter ScreenCast,
// picker-free) and via XComposite on X11. The candidate list is whatever the host
// enumerates (all installed apps on Wayland, running apps on X11 / macOS).

typedef struct {
    GtkWidget *dialog;
    GtkWidget *capture_all;
    GtkWidget *check_list;    // GtkBox (vertical) of one GtkCheckButton per available app
    GtkWidget *apps_section;  // greyed out while "capture all" is active
    // Extra precondition for enabling the dialog's accept button (the wizard sets this
    // when a required host component is missing); ANDed with the capture selection.
    gboolean required_unmet;
} SettingsCtx;

static guint settings_selected_count(SettingsCtx *c) {
    GList *kids = gtk_container_get_children(GTK_CONTAINER(c->check_list));
    guint n = 0;
    for (GList *l = kids; l; l = l->next) {
        if (GTK_IS_TOGGLE_BUTTON(l->data) &&
            gtk_toggle_button_get_active(GTK_TOGGLE_BUTTON(l->data))) {
            n++;
        }
    }
    g_list_free(kids);
    return n;
}

// Fail-closed accept gate: the accept button stays disabled while "capture all" is off
// AND no app has been added (a "capture nothing" config can never be saved), and also
// while any required host component is unmet (wizard).
static void settings_update_save(SettingsCtx *c) {
    gboolean all = gtk_toggle_button_get_active(GTK_TOGGLE_BUTTON(c->capture_all));
    gboolean ok = (all || settings_selected_count(c) > 0) && !c->required_unmet;
    gtk_dialog_set_response_sensitive(GTK_DIALOG(c->dialog), GTK_RESPONSE_OK, ok);
}

// A checklist row's tick changed: re-evaluate the accept gate (capture-all OR >=1 ticked).
static void on_settings_check_toggled(GtkToggleButton *btn, gpointer data) {
    (void)btn;
    settings_update_save((SettingsCtx *)data);
}

static void on_settings_capture_all(GtkToggleButton *btn, gpointer data) {
    SettingsCtx *ctx = (SettingsCtx *)data;
    gtk_widget_set_sensitive(ctx->apps_section,
                             !gtk_toggle_button_get_active(btn) && g_per_app_available);
    settings_update_save(ctx);
}

// Build the "Capture all" toggle + the add/remove app list into `content`, prefilling
// `preselected`. Fills *ctx (read by the caller on accept to collect the chosen apps)
// and wires the accept gate. `required_unmet` keeps accept disabled until host
// requirements are met (wizard); pass FALSE from the Settings dialog. Centralizes the
// g_per_app_available gate so both callers share one correct codepath.
static void build_addremove_section(GtkWidget *content, GtkWidget *dialog,
                                    gboolean initial_capture_all,
                                    const char **preselected, size_t preselected_count,
                                    gboolean required_unmet, SettingsCtx *ctx) {
    ctx->dialog = dialog;
    ctx->required_unmet = required_unmet;

    ctx->capture_all = gtk_check_button_new_with_label("Capture all applications");
    gtk_toggle_button_set_active(GTK_TOGGLE_BUTTON(ctx->capture_all), initial_capture_all);
    gtk_box_pack_start(GTK_BOX(content), ctx->capture_all, FALSE, FALSE, 0);

    ctx->apps_section = gtk_box_new(GTK_ORIENTATION_VERTICAL, 6);
    gtk_widget_set_vexpand(ctx->apps_section, TRUE);
    gtk_box_pack_start(GTK_BOX(content), ctx->apps_section, TRUE, TRUE, 0);

    GtkWidget *apps_label = gtk_label_new("Applications to capture:");
    gtk_label_set_xalign(GTK_LABEL(apps_label), 0.0);
    gtk_box_pack_start(GTK_BOX(ctx->apps_section), apps_label, FALSE, FALSE, 0);

    // Scrollable checklist: one tickable row per available app (ticked = captured),
    // mirroring the macOS panel. Apps already in the saved selection start ticked.
    GtkWidget *scroll = gtk_scrolled_window_new(NULL, NULL);
    gtk_scrolled_window_set_policy(GTK_SCROLLED_WINDOW(scroll),
        GTK_POLICY_NEVER, GTK_POLICY_AUTOMATIC);
    gtk_widget_set_vexpand(scroll, TRUE);
    ctx->check_list = gtk_box_new(GTK_ORIENTATION_VERTICAL, 2);
    gtk_container_add(GTK_CONTAINER(scroll), ctx->check_list);
    gtk_box_pack_start(GTK_BOX(ctx->apps_section), scroll, TRUE, TRUE, 0);

    for (size_t i = 0; i < g_app_count; i++) {
        if (!g_app_ids[i] || !g_app_ids[i][0]) {
            continue;
        }
        const char *label =
            (g_app_names[i] && g_app_names[i][0]) ? g_app_names[i] : g_app_ids[i];
        GtkWidget *chk = gtk_check_button_new_with_label(label);
        g_object_set_data_full(G_OBJECT(chk), "app-id", g_strdup(g_app_ids[i]), g_free);
        for (size_t j = 0; j < preselected_count; j++) {
            if (preselected[j] && strcmp(preselected[j], g_app_ids[i]) == 0) {
                gtk_toggle_button_set_active(GTK_TOGGLE_BUTTON(chk), TRUE);
                break;
            }
        }
        g_signal_connect(chk, "toggled", G_CALLBACK(on_settings_check_toggled), ctx);
        gtk_box_pack_start(GTK_BOX(ctx->check_list), chk, FALSE, FALSE, 0);
    }

    g_signal_connect(ctx->capture_all, "toggled", G_CALLBACK(on_settings_capture_all), ctx);

    if (!g_per_app_available) {
        // Per-app capture isn't available on this host -- force full-screen capture.
        gtk_toggle_button_set_active(GTK_TOGGLE_BUTTON(ctx->capture_all), TRUE);
        gtk_widget_set_sensitive(ctx->capture_all, FALSE);
        GtkWidget *cap_note = gtk_label_new(NULL);
        gtk_label_set_markup(GTK_LABEL(cap_note),
            "<i>Per-app capture isn't supported by your compositor (it only allows "
            "full-screen capture); the full screen will be captured.</i>");
        gtk_label_set_xalign(GTK_LABEL(cap_note), 0.0);
        gtk_label_set_line_wrap(GTK_LABEL(cap_note), TRUE);
        gtk_box_pack_start(GTK_BOX(content), cap_note, FALSE, FALSE, 0);
    }
    gtk_widget_set_sensitive(ctx->apps_section,
        !gtk_toggle_button_get_active(GTK_TOGGLE_BUTTON(ctx->capture_all)) && g_per_app_available);

    settings_update_save(ctx);
}

// Collect the chosen apps from the add/remove list into a freshly-allocated
// (strdup'd) array. Stores nothing when "capture all" is active. Shared by both
// dialogs' accept paths; the array + entries are freed by *_free_result().
static void settings_collect_selection(SettingsCtx *ctx, bool capture_all,
                                       const char ***out_apps, size_t *out_count) {
    *out_apps = NULL;
    *out_count = 0;
    if (capture_all) {
        return;
    }
    GList *kids = gtk_container_get_children(GTK_CONTAINER(ctx->check_list));
    guint n = g_list_length(kids);
    if (n > 0) {
        const char **sel = (const char **)calloc(n, sizeof(char *));
        size_t j = 0;
        for (GList *l = kids; l; l = l->next) {
            if (!GTK_IS_TOGGLE_BUTTON(l->data) ||
                !gtk_toggle_button_get_active(GTK_TOGGLE_BUTTON(l->data))) {
                continue; // only ticked apps are captured
            }
            const char *id = (const char *)g_object_get_data(G_OBJECT(l->data), "app-id");
            if (id && id[0]) {
                sel[j++] = strdup(id);
            }
        }
        *out_apps = sel;
        *out_count = j;
    }
    g_list_free(kids);
}

int wizard_run(WizardConfig *out) {
    if (!out) return 1;
    out->capture_all = false;
    out->enable_autostart = false;
    out->selected_apps = NULL;
    out->selected_apps_count = 0;
    out->completed = false;
    out->cancelled = true;

    if (!gtk_init_check(NULL, NULL)) {
        return 2;
    }

    GtkWidget *dialog = gtk_dialog_new_with_buttons(
        "crowd-cast setup", NULL, GTK_DIALOG_MODAL,
        "_Cancel", GTK_RESPONSE_CANCEL,
        "_Finish", GTK_RESPONSE_OK,
        NULL);
    gtk_window_set_default_size(GTK_WINDOW(dialog), 520, 560);

    GtkWidget *content = gtk_dialog_get_content_area(GTK_DIALOG(dialog));
    gtk_container_set_border_width(GTK_CONTAINER(content), 16);
    gtk_box_set_spacing(GTK_BOX(content), 10);

    GtkWidget *title = gtk_label_new(NULL);
    gtk_label_set_markup(GTK_LABEL(title),
        "<span size='x-large' weight='bold'>Welcome to crowd-cast</span>");
    gtk_label_set_xalign(GTK_LABEL(title), 0.0);
    gtk_box_pack_start(GTK_BOX(content), title, FALSE, FALSE, 0);

    GtkWidget *intro = gtk_label_new(
        "crowd-cast records your screen and input to build training data.");
    gtk_label_set_xalign(GTK_LABEL(intro), 0.0);
    gtk_label_set_line_wrap(GTK_LABEL(intro), TRUE);
    gtk_box_pack_start(GTK_BOX(content), intro, FALSE, FALSE, 0);

    // ---- System requirements section + hard Finish gate ----
    // A missing Required item (e.g. screen capture) is a hard gate: there is no
    // "continue anyway". Finish stays disabled until every Required item is met.
    gboolean required_unmet = FALSE;
    if (g_req_count > 0) {
        GtkWidget *frame = gtk_frame_new("System requirements");
        GtkWidget *reqbox = gtk_box_new(GTK_ORIENTATION_VERTICAL, 6);
        gtk_container_set_border_width(GTK_CONTAINER(reqbox), 8);
        gtk_container_add(GTK_CONTAINER(frame), reqbox);
        for (size_t i = 0; i < g_req_count; i++) {
            if (g_req_sev[i] == 0 && !g_req_sat[i]) {
                required_unmet = TRUE;
            }
            gtk_box_pack_start(GTK_BOX(reqbox),
                make_req_row(g_req_labels[i], g_req_details[i], g_req_cmds[i], g_req_sev[i], g_req_sat[i]),
                FALSE, FALSE, 0);
        }
        gtk_box_pack_start(GTK_BOX(content), frame, FALSE, FALSE, 0);

        if (required_unmet) {
            GtkWidget *note = gtk_label_new(NULL);
            gtk_label_set_markup(GTK_LABEL(note),
                "<span foreground='#c62828'>A required component isn't ready. Follow the instructions "
                "shown for each flagged item above, then re-run setup. Some changes (such as installing "
                "the GNOME focus extension or joining the input group) only take effect after you log "
                "out and back in. Setup cannot continue until they are resolved.</span>");
            gtk_label_set_xalign(GTK_LABEL(note), 0.0);
            gtk_label_set_line_wrap(GTK_LABEL(note), TRUE);
            gtk_box_pack_start(GTK_BOX(content), note, FALSE, FALSE, 0);

            gtk_dialog_set_response_sensitive(GTK_DIALOG(dialog), GTK_RESPONSE_OK, FALSE);
        }
    }

    // ---- Capture options (shared add/remove UI with the tray "Settings" dialog) ----
    // Start with NO capture mode selected: "capture all" defaults off and the app list
    // is empty, so settings_update_save() keeps Finish disabled until the user makes an
    // explicit choice (tick "capture all" or add an app). We must never silently default
    // to recording the whole screen. (When per-app capture is unavailable on the host,
    // build_addremove_section forces "capture all" on with a visible note -- that's an
    // explicit, gated choice, not a silent default.)
    SettingsCtx ctx;
    build_addremove_section(content, dialog, FALSE, NULL, 0, required_unmet, &ctx);

    GtkWidget *autostart = gtk_check_button_new_with_label("Start crowd-cast on login");
    gtk_box_pack_start(GTK_BOX(content), autostart, FALSE, FALSE, 0);

    gtk_widget_show_all(dialog);
    gint resp = gtk_dialog_run(GTK_DIALOG(dialog));

    if (resp == GTK_RESPONSE_OK) {
        out->completed = true;
        out->cancelled = false;
        out->capture_all = gtk_toggle_button_get_active(GTK_TOGGLE_BUTTON(ctx.capture_all));
        out->enable_autostart = gtk_toggle_button_get_active(GTK_TOGGLE_BUTTON(autostart));
        settings_collect_selection(&ctx, out->capture_all,
                                   &out->selected_apps, &out->selected_apps_count);
    }

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

// ---- Tray "Settings" dialog -------------------------------------------------
// The Linux analog of the macOS Cocoa app-selection panel. Uses the shared add/remove
// capture-mode UI (build_addremove_section, above) -- the same UI the first-run wizard
// uses. Consumed via show_panel() in src/ui/app_selector.rs; the SAME C ABI
// (show_app_selection_panel / AppSelectionResult) is implemented by
// src/ui/wizard_darwin.m on macOS.
void show_app_selection_panel(const char **current_apps, size_t current_count,
                              bool capture_all, AppSelectionResult *out) {
    if (!out) return;
    out->capture_all = capture_all;
    out->selected_apps = NULL;
    out->selected_apps_count = 0;
    out->saved = false;

    // Idempotent: a no-op if the setup wizard already initialized GTK earlier in this
    // process, and the first init if the agent launched straight into the tray.
    if (!gtk_init_check(NULL, NULL)) {
        return;
    }

    GtkWidget *dialog = gtk_dialog_new_with_buttons(
        "crowd-cast settings", NULL, GTK_DIALOG_MODAL,
        "_Cancel", GTK_RESPONSE_CANCEL,
        "_Save", GTK_RESPONSE_OK,
        NULL);
    gtk_window_set_default_size(GTK_WINDOW(dialog), 520, 480);

    GtkWidget *content = gtk_dialog_get_content_area(GTK_DIALOG(dialog));
    gtk_container_set_border_width(GTK_CONTAINER(content), 16);
    gtk_box_set_spacing(GTK_BOX(content), 10);

    GtkWidget *title = gtk_label_new(NULL);
    gtk_label_set_markup(GTK_LABEL(title),
        "<span size='x-large' weight='bold'>Applications to capture</span>");
    gtk_label_set_xalign(GTK_LABEL(title), 0.0);
    gtk_box_pack_start(GTK_BOX(content), title, FALSE, FALSE, 0);

    GtkWidget *intro = gtk_label_new(
        "Add the applications crowd-cast should record. On Save, crowd-cast restarts and "
        "asks you (through the desktop portal) to pick each newly added app's window. "
        "Input is recorded only while one of these apps is focused.");
    gtk_label_set_xalign(GTK_LABEL(intro), 0.0);
    gtk_label_set_line_wrap(GTK_LABEL(intro), TRUE);
    gtk_box_pack_start(GTK_BOX(content), intro, FALSE, FALSE, 0);

    SettingsCtx ctx;
    build_addremove_section(content, dialog, capture_all ? TRUE : FALSE,
                            current_apps, current_count, FALSE, &ctx);

    gtk_widget_show_all(dialog);
    gint resp = gtk_dialog_run(GTK_DIALOG(dialog));

    if (resp == GTK_RESPONSE_OK) {
        out->saved = true;
        out->capture_all = gtk_toggle_button_get_active(GTK_TOGGLE_BUTTON(ctx.capture_all));
        settings_collect_selection(&ctx, out->capture_all,
                                   &out->selected_apps, &out->selected_apps_count);
    }

    gtk_widget_destroy(dialog);
    while (gtk_events_pending()) gtk_main_iteration();
}

void app_selection_free_result(AppSelectionResult *out) {
    if (!out || !out->selected_apps) return;
    for (size_t i = 0; i < out->selected_apps_count; i++) {
        free((void *)out->selected_apps[i]);
    }
    free((void *)out->selected_apps);
    out->selected_apps = NULL;
    out->selected_apps_count = 0;
}
