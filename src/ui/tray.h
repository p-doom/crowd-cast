/*
 * Tray icon library - Cross-platform system tray implementation
 * Based on dmikushin/tray (https://github.com/dmikushin/tray)
 * MIT License
 */

#ifndef TRAY_H
#define TRAY_H

#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

struct tray_menu;

struct tray {
    const char *icon_filepath;
    const char *tooltip;
    void (*cb)(struct tray *tray);
    struct tray_menu *menu;
};

struct tray_menu {
    const char *text;
    int disabled;
    int checked;
    void (*cb)(struct tray_menu *item);
    struct tray_menu *submenu;
};

/* Initialize the tray icon and menu */
int tray_init(struct tray *tray);

/* Run one iteration of the event loop */
/* If blocking is non-zero, blocks until an event occurs */
/* Returns 0 normally, -1 if tray_exit() was called */
int tray_loop(int blocking);

/* Update the tray icon, tooltip, and menu */
void tray_update(struct tray *tray);

/* Tear down AppKit tray state before replacing the process */
void tray_prepare_for_restart(void);

/* Signal the event loop to exit */
void tray_exit(void);

/* Returns true once if the screen was unlocked since last check */
bool tray_screen_was_unlocked(void);

#ifdef __cplusplus
}
#endif

#endif /* TRAY_H */
