/*
 * Tray icon library - Cross-platform system tray implementation
 * Based on dmikushin/tray (https://github.com/dmikushin/tray)
 * MIT License
 */

#ifndef TRAY_H
#define TRAY_H

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

/* Signal the event loop to exit */
void tray_exit(void);

#ifdef __cplusplus
}
#endif

#endif /* TRAY_H */
