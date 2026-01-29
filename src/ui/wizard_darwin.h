/*
 * Native macOS Setup Wizard
 * Provides a native Cocoa UI for the first-run setup experience
 */

#ifndef WIZARD_DARWIN_H
#define WIZARD_DARWIN_H

#ifdef __cplusplus
extern "C" {
#endif

#include <stdint.h>
#include <stdbool.h>

// App info structure passed from Rust
typedef struct {
    const char *bundle_id;
    const char *name;
    uint32_t pid;
} WizardAppInfo;

// Configuration structure for wizard results
typedef struct {
    bool capture_all;
    bool enable_autostart;
    // Selected app bundle IDs - array of C strings
    const char **selected_apps;
    size_t selected_apps_count;
    // Output flags
    bool completed;
    bool cancelled;
} WizardConfig;

// Set the list of available apps for selection
// apps: Array of WizardAppInfo structures
// count: Number of apps in the array
void wizard_set_apps(const WizardAppInfo *apps, size_t count);

// Run the setup wizard
// config: Pointer to WizardConfig that will be filled with results
// Returns: 0 on success, -1 on error
int wizard_run(WizardConfig *config);

// Free any memory allocated by the wizard for selected_apps
void wizard_free_result(WizardConfig *config);

// Check accessibility permission status
// Returns: 1 if granted, 0 if denied
int wizard_check_accessibility(void);

// Check screen recording permission status
// Returns: 1 if granted, 0 if denied
int wizard_check_screen_recording(void);

// Request accessibility permission (shows system prompt)
// Returns: 1 if granted after prompt, 0 if denied
int wizard_request_accessibility(void);

// Request screen recording permission (shows system prompt)
// Returns: 1 if granted after prompt, 0 if denied
int wizard_request_screen_recording(void);

// Open System Preferences to Accessibility pane
void wizard_open_accessibility_settings(void);

// Open System Preferences to Screen Recording pane
void wizard_open_screen_recording_settings(void);

// Check notification permission status
// Returns: 1 if granted, 0 if denied
int wizard_check_notifications(void);

// Request notification permission (shows system prompt)
void wizard_request_notifications(void);

// Open System Preferences to Notifications pane
void wizard_open_notifications_settings(void);

#ifdef __cplusplus
}
#endif

#endif /* WIZARD_DARWIN_H */
