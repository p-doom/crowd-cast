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

// Grey out the per-app list while "capture all" is active, or whenever per-app capture
// isn't available on this host (mutually exclusive with "capture all").
static void on_capture_all_toggled(GtkToggleButton *btn, gpointer apps_section) {
    gtk_widget_set_sensitive(GTK_WIDGET(apps_section),
                             !gtk_toggle_button_get_active(btn) && g_per_app_available);
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
                "<span foreground='#c62828'>A required component is missing. Install the package each "
                "item names above, then re-run setup. Setup cannot continue until it is resolved.</span>");
            gtk_label_set_xalign(GTK_LABEL(note), 0.0);
            gtk_label_set_line_wrap(GTK_LABEL(note), TRUE);
            gtk_box_pack_start(GTK_BOX(content), note, FALSE, FALSE, 0);

            gtk_dialog_set_response_sensitive(GTK_DIALOG(dialog), GTK_RESPONSE_OK, FALSE);
        }
    }

    // ---- Capture options ----
    GtkWidget *capture_all = gtk_check_button_new_with_label(
        "Capture all applications");
    gtk_toggle_button_set_active(GTK_TOGGLE_BUTTON(capture_all), TRUE);
    gtk_box_pack_start(GTK_BOX(content), capture_all, FALSE, FALSE, 0);

    // Per-app selection lives in its own section that is disabled (greyed out) while
    // "capture all" is active, enforcing mutual exclusivity.
    GtkWidget *apps_section = gtk_box_new(GTK_ORIENTATION_VERTICAL, 4);
    gtk_widget_set_vexpand(apps_section, TRUE);
    gtk_box_pack_start(GTK_BOX(content), apps_section, TRUE, TRUE, 0);

    GtkWidget *apps_label = gtk_label_new("Or select specific applications:");
    gtk_label_set_xalign(GTK_LABEL(apps_label), 0.0);
    gtk_box_pack_start(GTK_BOX(apps_section), apps_label, FALSE, FALSE, 0);

    GtkWidget *scroll = gtk_scrolled_window_new(NULL, NULL);
    gtk_scrolled_window_set_policy(GTK_SCROLLED_WINDOW(scroll),
        GTK_POLICY_NEVER, GTK_POLICY_AUTOMATIC);
    gtk_widget_set_vexpand(scroll, TRUE);
    GtkWidget *apps_box = gtk_box_new(GTK_ORIENTATION_VERTICAL, 4);
    gtk_container_add(GTK_CONTAINER(scroll), apps_box);
    gtk_box_pack_start(GTK_BOX(apps_section), scroll, TRUE, TRUE, 0);

    g_signal_connect(capture_all, "toggled", G_CALLBACK(on_capture_all_toggled), apps_section);
    gtk_widget_set_sensitive(apps_section,
        !gtk_toggle_button_get_active(GTK_TOGGLE_BUTTON(capture_all)) && g_per_app_available);

    if (!g_per_app_available) {
        // Per-app capture isn't available on this host -- force full-screen capture and
        // grey out the per-app picker so an unusable config can't be created.
        gtk_toggle_button_set_active(GTK_TOGGLE_BUTTON(capture_all), TRUE);
        gtk_widget_set_sensitive(capture_all, FALSE);
        gtk_widget_set_sensitive(apps_section, FALSE);
        GtkWidget *cap_note = gtk_label_new(NULL);
        gtk_label_set_markup(GTK_LABEL(cap_note),
            "<i>Per-app capture isn't supported by your compositor (it only allows "
            "full-screen capture); the full screen will be captured.</i>");
        gtk_label_set_xalign(GTK_LABEL(cap_note), 0.0);
        gtk_label_set_line_wrap(GTK_LABEL(cap_note), TRUE);
        gtk_box_pack_start(GTK_BOX(content), cap_note, FALSE, FALSE, 0);
    }

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
