/*
 * Linux platform implementation for frontmost application detection.
 * 
 * X11: Uses _NET_ACTIVE_WINDOW property to get the active window,
 *      then WM_CLASS to get the application identifier.
 * 
 * Wayland: Cannot detect frontmost app due to security model.
 *          Returns NULL, triggering fallback to manual toggle.
 */

#if !defined(_WIN32) && !defined(__APPLE__)

#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <stdbool.h>
#include "platform.h"

/* Check for Wayland vs X11 session */
bool platform_is_wayland(void)
{
    const char *session = getenv("XDG_SESSION_TYPE");
    if (session && strcmp(session, "wayland") == 0) {
        return true;
    }
    
    /* Also check for WAYLAND_DISPLAY as backup */
    const char *wayland_display = getenv("WAYLAND_DISPLAY");
    if (wayland_display && strlen(wayland_display) > 0) {
        /* Only consider Wayland if DISPLAY is not set (pure Wayland) 
         * or if XDG_SESSION_TYPE explicitly says wayland */
        const char *x_display = getenv("DISPLAY");
        if (!x_display || strlen(x_display) == 0) {
            return true;
        }
    }
    
    return false;
}

#ifdef HAVE_X11
#include <X11/Xlib.h>
#include <X11/Xatom.h>

static Display *get_display(void)
{
    static Display *display = NULL;
    if (!display) {
        display = XOpenDisplay(NULL);
    }
    return display;
}

static Window get_active_window(Display *display)
{
    if (!display) return None;
    
    Atom actual_type;
    int actual_format;
    unsigned long nitems, bytes_after;
    unsigned char *prop = NULL;
    Window active = None;
    
    Atom net_active = XInternAtom(display, "_NET_ACTIVE_WINDOW", False);
    Window root = DefaultRootWindow(display);
    
    if (XGetWindowProperty(display, root, net_active, 0, 1, False,
                           XA_WINDOW, &actual_type, &actual_format,
                           &nitems, &bytes_after, &prop) == Success) {
        if (prop && nitems > 0) {
            active = *((Window *)prop);
        }
        if (prop) XFree(prop);
    }
    
    return active;
}

static char *get_window_class(Display *display, Window window)
{
    if (!display || window == None) return NULL;
    
    XClassHint class_hint;
    if (XGetClassHint(display, window, &class_hint)) {
        char *result = NULL;
        if (class_hint.res_class) {
            result = strdup(class_hint.res_class);
            XFree(class_hint.res_class);
        }
        if (class_hint.res_name) {
            XFree(class_hint.res_name);
        }
        return result;
    }
    
    return NULL;
}

char *platform_get_frontmost_app_id(void)
{
    if (platform_is_wayland()) {
        /* Wayland: cannot determine frontmost app */
        return NULL;
    }
    
    Display *display = get_display();
    if (!display) {
        return NULL;
    }
    
    Window active = get_active_window(display);
    if (active == None) {
        return NULL;
    }
    
    return get_window_class(display, active);
}

#else /* !HAVE_X11 */

char *platform_get_frontmost_app_id(void)
{
    /* No X11 support compiled in, and Wayland can't detect frontmost app */
    return NULL;
}

#endif /* HAVE_X11 */

bool platform_app_ids_match(const char *frontmost_id, const char *target_id)
{
    if (!frontmost_id || !target_id) {
        return false;
    }
    
    /*
     * On Linux, the frontmost_id is the WM_CLASS (e.g., "code", "firefox").
     * The target_id from window capture sources varies by capture type:
     * - xcomposite_input: window title or ID
     * - pipewire: varies by portal implementation
     * 
     * We do case-insensitive comparison and substring matching.
     */
    
    /* Direct case-insensitive match */
    if (strcasecmp(frontmost_id, target_id) == 0) {
        return true;
    }
    
    /* Check if either contains the other (case-insensitive) */
    char *front_lower = strdup(frontmost_id);
    char *target_lower = strdup(target_id);
    
    if (front_lower && target_lower) {
        for (char *p = front_lower; *p; p++) *p = tolower((unsigned char)*p);
        for (char *p = target_lower; *p; p++) *p = tolower((unsigned char)*p);
        
        bool found = strstr(target_lower, front_lower) != NULL ||
                     strstr(front_lower, target_lower) != NULL;
        
        free(front_lower);
        free(target_lower);
        return found;
    }
    
    free(front_lower);
    free(target_lower);
    return false;
}

#endif /* !_WIN32 && !__APPLE__ */
