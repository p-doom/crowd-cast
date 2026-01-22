/*
 * OBS CrowdCast Plugin
 * 
 * Exposes window capture functionality via obs-websocket vendor requests:
 * 
 * 1. crowdcast.GetHookedSources
 *    Returns the "hooked" state of window capture sources (whether OBS is 
 *    actively capturing a window vs showing a black frame).
 *    Response: { "sources": [...], "any_hooked": true }
 * 
 * 2. crowdcast.GetAvailableWindows
 *    Enumerates all available windows that can be captured.
 *    Response: { "windows": [...], "suggested": [...], "source_type": "..." }
 * 
 * 3. crowdcast.CreateCaptureSources
 *    Creates window capture sources for selected windows.
 *    Request: { "windows": [{"id": "...", "name": "..."}] }
 *    Response: { "success": true, "created_count": 3, ... }
 */

#include <obs-module.h>
#include <obs-frontend-api.h>
#include <util/dstr.h>
#include <util/threading.h>
#include <ctype.h>
#include <stdlib.h>

/* Official obs-websocket vendor API header (uses proc_handler, not dlsym) */
#include "obs-websocket-api/obs-websocket-api.h"

OBS_DECLARE_MODULE()
OBS_MODULE_USE_DEFAULT_LOCALE("obs-crowdcast", "en-US")

MODULE_EXPORT const char *obs_module_description(void)
{
    return "CrowdCast Plugin - Window capture state, enumeration, and source creation via obs-websocket";
}

/* ========================================================================== */
/* Source State Tracking                                                       */
/* ========================================================================== */

#define MAX_TRACKED_SOURCES 64

typedef struct source_state {
    char name[256];
    bool hooked;
    bool active;
    bool in_use;
} source_state_t;

static source_state_t g_sources[MAX_TRACKED_SOURCES];
static size_t g_source_count = 0;
static pthread_mutex_t g_state_mutex = PTHREAD_MUTEX_INITIALIZER;
static obs_websocket_vendor g_vendor = NULL;

/* Find or create a source state entry */
static source_state_t *find_or_create_source(const char *name)
{
    /* First, try to find existing */
    for (size_t i = 0; i < g_source_count; i++) {
        if (g_sources[i].in_use && strcmp(g_sources[i].name, name) == 0) {
            return &g_sources[i];
        }
    }
    
    /* Create new entry */
    if (g_source_count < MAX_TRACKED_SOURCES) {
        source_state_t *s = &g_sources[g_source_count++];
        strncpy(s->name, name, sizeof(s->name) - 1);
        s->name[sizeof(s->name) - 1] = '\0';
        s->hooked = false;
        s->active = false;
        s->in_use = true;
        return s;
    }
    
    return NULL;
}

static source_state_t *find_source(const char *name)
{
    for (size_t i = 0; i < g_source_count; i++) {
        if (g_sources[i].in_use && strcmp(g_sources[i].name, name) == 0) {
            return &g_sources[i];
        }
    }
    return NULL;
}

static void remove_source(const char *name)
{
    for (size_t i = 0; i < g_source_count; i++) {
        if (g_sources[i].in_use && strcmp(g_sources[i].name, name) == 0) {
            g_sources[i].in_use = false;
            return;
        }
    }
}

/* ========================================================================== */
/* Signal Handlers                                                             */
/* ========================================================================== */

static void on_source_hooked(void *data, calldata_t *cd)
{
    UNUSED_PARAMETER(data);
    obs_source_t *source = calldata_ptr(cd, "source");
    if (!source)
        return;
    
    const char *name = obs_source_get_name(source);
    if (!name)
        return;
    
    pthread_mutex_lock(&g_state_mutex);
    source_state_t *state = find_source(name);
    if (state) {
        state->hooked = true;
        blog(LOG_DEBUG, "[crowdcast] Source '%s' hooked", name);
    }
    pthread_mutex_unlock(&g_state_mutex);
}

static void on_source_unhooked(void *data, calldata_t *cd)
{
    UNUSED_PARAMETER(data);
    obs_source_t *source = calldata_ptr(cd, "source");
    if (!source)
        return;
    
    const char *name = obs_source_get_name(source);
    if (!name)
        return;
    
    pthread_mutex_lock(&g_state_mutex);
    source_state_t *state = find_source(name);
    if (state) {
        state->hooked = false;
        blog(LOG_DEBUG, "[crowdcast] Source '%s' unhooked", name);
    }
    pthread_mutex_unlock(&g_state_mutex);
}

static void on_source_activate(void *data, calldata_t *cd)
{
    UNUSED_PARAMETER(data);
    obs_source_t *source = calldata_ptr(cd, "source");
    if (!source)
        return;
    
    const char *name = obs_source_get_name(source);
    if (!name)
        return;
    
    pthread_mutex_lock(&g_state_mutex);
    source_state_t *state = find_source(name);
    if (state) {
        state->active = true;
    }
    pthread_mutex_unlock(&g_state_mutex);
}

static void on_source_deactivate(void *data, calldata_t *cd)
{
    UNUSED_PARAMETER(data);
    obs_source_t *source = calldata_ptr(cd, "source");
    if (!source)
        return;
    
    const char *name = obs_source_get_name(source);
    if (!name)
        return;
    
    pthread_mutex_lock(&g_state_mutex);
    source_state_t *state = find_source(name);
    if (state) {
        state->active = false;
    }
    pthread_mutex_unlock(&g_state_mutex);
}

/* ========================================================================== */
/* Source Registration                                                         */
/* ========================================================================== */

static bool is_window_capture_source(obs_source_t *source)
{
    const char *id = obs_source_get_id(source);
    if (!id)
        return false;
    
    /* Check for various window capture source types across platforms */
    return strcmp(id, "window_capture") == 0 ||           /* Windows */
           strcmp(id, "xcomposite_input") == 0 ||         /* Linux X11 */
           strcmp(id, "pipewire-screen-capture-source") == 0 || /* Linux PipeWire */
           strcmp(id, "coreaudio_input_capture") == 0 ||  /* macOS */
           strstr(id, "window") != NULL;                  /* Fallback */
}

static void register_source_signals(obs_source_t *source)
{
    if (!is_window_capture_source(source))
        return;
    
    const char *name = obs_source_get_name(source);
    if (!name)
        return;
    
    pthread_mutex_lock(&g_state_mutex);
    source_state_t *state = find_or_create_source(name);
    if (state) {
        state->active = obs_source_active(source);
        /* Initial hooked state - assume hooked if source exists and is configured */
        state->hooked = false;
    }
    pthread_mutex_unlock(&g_state_mutex);
    
    signal_handler_t *sh = obs_source_get_signal_handler(source);
    if (sh) {
        /* Note: "hooked" and "unhooked" signals are specific to window capture on Windows.
         * On other platforms, we may need to use different detection methods.
         * For now, we register for these signals and also track activate/deactivate. */
        signal_handler_connect(sh, "hooked", on_source_hooked, NULL);
        signal_handler_connect(sh, "unhooked", on_source_unhooked, NULL);
        signal_handler_connect(sh, "activate", on_source_activate, NULL);
        signal_handler_connect(sh, "deactivate", on_source_deactivate, NULL);
        
        blog(LOG_INFO, "[crowdcast] Registered signals for source '%s'", name);
    }
}

static void unregister_source_signals(obs_source_t *source)
{
    const char *name = obs_source_get_name(source);
    if (!name)
        return;
    
    signal_handler_t *sh = obs_source_get_signal_handler(source);
    if (sh) {
        signal_handler_disconnect(sh, "hooked", on_source_hooked, NULL);
        signal_handler_disconnect(sh, "unhooked", on_source_unhooked, NULL);
        signal_handler_disconnect(sh, "activate", on_source_activate, NULL);
        signal_handler_disconnect(sh, "deactivate", on_source_deactivate, NULL);
    }
    
    pthread_mutex_lock(&g_state_mutex);
    remove_source(name);
    pthread_mutex_unlock(&g_state_mutex);
}

/* ========================================================================== */
/* Source Enumeration                                                          */
/* ========================================================================== */

static bool enum_sources_cb(void *data, obs_source_t *source)
{
    UNUSED_PARAMETER(data);
    register_source_signals(source);
    return true;
}

static void enumerate_existing_sources(void)
{
    obs_enum_sources(enum_sources_cb, NULL);
}

/* ========================================================================== */
/* Global Source Add/Remove Handlers                                           */
/* ========================================================================== */

static void on_source_created(void *data, calldata_t *cd)
{
    UNUSED_PARAMETER(data);
    obs_source_t *source = calldata_ptr(cd, "source");
    if (source) {
        register_source_signals(source);
    }
}

static void on_source_removed(void *data, calldata_t *cd)
{
    UNUSED_PARAMETER(data);
    obs_source_t *source = calldata_ptr(cd, "source");
    if (source) {
        unregister_source_signals(source);
    }
}

/* ========================================================================== */
/* Suggested Applications List                                                 */
/* ========================================================================== */

/* List of app names to suggest for capture (case-insensitive matching) */
static const char *g_suggested_apps[] = {
    /* Browsers */
    "firefox", "chrome", "chromium", "safari", "brave", "edge", "opera", "vivaldi",
    /* IDEs and Editors */
    "cursor", "code", "codium", "idea", "webstorm", "pycharm", "goland", "clion",
    "sublime_text", "sublime", "atom", "vim", "nvim", "emacs", "notepad++",
    /* PDF and Document Viewers */
    "preview", "evince", "okular", "acrobat", "reader", "foxit", "zathura",
    /* Terminals */
    "terminal", "iterm", "iterm2", "alacritty", "kitty", "wezterm", "hyper",
    "gnome-terminal", "konsole", "xterm",
    NULL  /* Sentinel */
};

/* Check if an app name matches any suggested app (case-insensitive) */
static bool is_suggested_app(const char *app_name)
{
    if (!app_name)
        return false;
    
    /* Convert to lowercase for comparison */
    char lower_name[256];
    size_t len = strlen(app_name);
    if (len >= sizeof(lower_name))
        len = sizeof(lower_name) - 1;
    
    for (size_t i = 0; i < len; i++) {
        lower_name[i] = tolower((unsigned char)app_name[i]);
    }
    lower_name[len] = '\0';
    
    for (const char **suggested = g_suggested_apps; *suggested != NULL; suggested++) {
        if (strstr(lower_name, *suggested) != NULL) {
            return true;
        }
    }
    
    return false;
}

/* ========================================================================== */
/* Platform-specific Window Capture Source Type                                */
/* ========================================================================== */

static const char *get_window_capture_source_id(void)
{
#ifdef _WIN32
    return "window_capture";
#elif defined(__APPLE__)
    /* Use ScreenCaptureKit-based capture for application capture */
    return "screen_capture";
#else
    /* Linux - check for Wayland vs X11 */
    const char *session_type = getenv("XDG_SESSION_TYPE");
    if (session_type && strcmp(session_type, "wayland") == 0) {
        return "pipewire-screen-capture-source";
    }
    return "xcomposite_input";
#endif
}

/* Get the capture type for ScreenCaptureKit on macOS
 * 0 = Display, 1 = Window, 2 = Application */
static int get_capture_type(void)
{
#ifdef __APPLE__
    return 2;  /* ScreenCaptureApplicationStream - capture entire application */
#else
    return -1; /* Not applicable on other platforms */
#endif
}

static const char *get_window_property_name(void)
{
#ifdef _WIN32
    return "window";
#elif defined(__APPLE__)
    /* For application capture, use the "application" property */
    return "application";
#else
    const char *session_type = getenv("XDG_SESSION_TYPE");
    if (session_type && strcmp(session_type, "wayland") == 0) {
        return "window";  /* PipeWire portal handles this differently */
    }
    return "capture_window";
#endif
}

/* ========================================================================== */
/* obs-websocket Vendor Request Handlers                                       */
/* ========================================================================== */

static void get_hooked_sources_cb(obs_data_t *request_data, obs_data_t *response_data, 
                                   void *priv_data)
{
    UNUSED_PARAMETER(request_data);
    UNUSED_PARAMETER(priv_data);
    
    obs_data_array_t *sources_array = obs_data_array_create();
    bool any_hooked = false;
    
    pthread_mutex_lock(&g_state_mutex);
    
    for (size_t i = 0; i < g_source_count; i++) {
        if (!g_sources[i].in_use)
            continue;
        
        obs_data_t *source_obj = obs_data_create();
        obs_data_set_string(source_obj, "name", g_sources[i].name);
        obs_data_set_bool(source_obj, "hooked", g_sources[i].hooked);
        obs_data_set_bool(source_obj, "active", g_sources[i].active);
        obs_data_array_push_back(sources_array, source_obj);
        obs_data_release(source_obj);
        
        if (g_sources[i].hooked && g_sources[i].active) {
            any_hooked = true;
        }
    }
    
    pthread_mutex_unlock(&g_state_mutex);
    
    obs_data_set_array(response_data, "sources", sources_array);
    obs_data_set_bool(response_data, "any_hooked", any_hooked);
    obs_data_array_release(sources_array);
}

/* ========================================================================== */
/* GetAvailableWindows Vendor Request                                          */
/* ========================================================================== */

static void get_available_windows_cb(obs_data_t *request_data, obs_data_t *response_data,
                                      void *priv_data)
{
    UNUSED_PARAMETER(request_data);
    UNUSED_PARAMETER(priv_data);
    
    obs_data_array_t *windows_array = obs_data_array_create();
    obs_data_array_t *suggested_array = obs_data_array_create();
    
    const char *source_id = get_window_capture_source_id();
    const char *window_prop = get_window_property_name();
    
    int capture_type = get_capture_type();
    blog(LOG_INFO, "[crowdcast] Enumerating using source type: %s, property: %s, capture_type: %d",
         source_id, window_prop, capture_type);
    
    /* Create a temporary source to access its properties */
    obs_data_t *settings = obs_data_create();
#ifdef __APPLE__
    /* Set capture type for ScreenCaptureKit (2 = Application capture) */
    obs_data_set_int(settings, "type", capture_type);
    /* Show hidden windows and applications for better enumeration */
    obs_data_set_bool(settings, "show_hidden_windows", true);
#endif
    obs_source_t *temp_source = obs_source_create_private(source_id, "crowdcast_temp", settings);
    obs_data_release(settings);
    
    if (!temp_source) {
        blog(LOG_WARNING, "[crowdcast] Failed to create temporary source for window enumeration");
        obs_data_set_array(response_data, "windows", windows_array);
        obs_data_set_array(response_data, "suggested", suggested_array);
        obs_data_array_release(windows_array);
        obs_data_array_release(suggested_array);
        return;
    }
    
    /* Get the properties object from the source */
    obs_properties_t *props = obs_source_properties(temp_source);
    if (!props) {
        blog(LOG_WARNING, "[crowdcast] Failed to get source properties");
        obs_source_release(temp_source);
        obs_data_set_array(response_data, "windows", windows_array);
        obs_data_set_array(response_data, "suggested", suggested_array);
        obs_data_array_release(windows_array);
        obs_data_array_release(suggested_array);
        return;
    }
    
    /* Find the window property and enumerate its list items */
    obs_property_t *window_property = obs_properties_get(props, window_prop);
    if (!window_property) {
        /* Try alternative property names */
        window_property = obs_properties_get(props, "window");
        if (!window_property) {
            window_property = obs_properties_get(props, "capture_window");
        }
    }
    
    if (window_property && obs_property_get_type(window_property) == OBS_PROPERTY_LIST) {
        size_t count = obs_property_list_item_count(window_property);
        blog(LOG_INFO, "[crowdcast] Found %zu windows", count);
        
        for (size_t i = 0; i < count; i++) {
            const char *item_name = obs_property_list_item_name(window_property, i);
            const char *item_value = obs_property_list_item_string(window_property, i);
            
            if (!item_name || !item_value || strlen(item_value) == 0)
                continue;
            
            /* Skip empty/placeholder entries */
            if (strcmp(item_name, "") == 0 || strcmp(item_name, "None") == 0)
                continue;
            
            obs_data_t *window_obj = obs_data_create();
            obs_data_set_string(window_obj, "id", item_value);
            obs_data_set_string(window_obj, "title", item_name);
            
            /* Try to extract app name from the window title/id */
            /* Format varies by platform, but often includes process name */
            char app_name[256] = "";
            
            /* On Windows/macOS, the id often contains the executable name */
            /* On Linux X11, format is typically "0xHEX WindowClass" */
            strncpy(app_name, item_name, sizeof(app_name) - 1);
            
            /* Extract just the app name part (before any dash or colon) */
            char *separator = strstr(app_name, " - ");
            if (!separator) separator = strstr(app_name, " — ");
            if (!separator) separator = strchr(app_name, ':');
            if (separator) *separator = '\0';
            
            /* Trim trailing whitespace */
            size_t len = strlen(app_name);
            while (len > 0 && (app_name[len-1] == ' ' || app_name[len-1] == '\t')) {
                app_name[--len] = '\0';
            }
            
            obs_data_set_string(window_obj, "app_name", app_name);
            
            /* Check if this is a suggested app */
            bool suggested = is_suggested_app(app_name) || is_suggested_app(item_name);
            obs_data_set_bool(window_obj, "suggested", suggested);
            
            obs_data_array_push_back(windows_array, window_obj);
            
            /* Also add to suggested list if it matches */
            if (suggested) {
                obs_data_t *sugg_obj = obs_data_create();
                obs_data_set_string(sugg_obj, "id", item_value);
                obs_data_set_string(sugg_obj, "title", item_name);
                obs_data_set_string(sugg_obj, "app_name", app_name);
                obs_data_set_bool(sugg_obj, "suggested", true);
                obs_data_array_push_back(suggested_array, sugg_obj);
                obs_data_release(sugg_obj);
            }
            
            obs_data_release(window_obj);
        }
    } else {
        blog(LOG_WARNING, "[crowdcast] Window property not found or not a list");
    }
    
    obs_properties_destroy(props);
    obs_source_release(temp_source);
    
    obs_data_set_array(response_data, "windows", windows_array);
    obs_data_set_array(response_data, "suggested", suggested_array);
    obs_data_set_string(response_data, "source_type", source_id);
    obs_data_set_string(response_data, "window_property", window_prop);
    
    obs_data_array_release(windows_array);
    obs_data_array_release(suggested_array);
    
    blog(LOG_INFO, "[crowdcast] GetAvailableWindows completed");
}

/* ========================================================================== */
/* CreateCaptureSources Vendor Request                                         */
/* ========================================================================== */

static void create_capture_sources_cb(obs_data_t *request_data, obs_data_t *response_data,
                                       void *priv_data)
{
    UNUSED_PARAMETER(priv_data);
    
    obs_data_array_t *created_array = obs_data_array_create();
    obs_data_array_t *failed_array = obs_data_array_create();
    int success_count = 0;
    int fail_count = 0;
    
    const char *source_id = get_window_capture_source_id();
    const char *window_prop = get_window_property_name();
    
    /* Get the windows array from the request */
    obs_data_array_t *windows = obs_data_get_array(request_data, "windows");
    if (!windows) {
        blog(LOG_WARNING, "[crowdcast] CreateCaptureSources: no 'windows' array in request");
        obs_data_set_bool(response_data, "success", false);
        obs_data_set_string(response_data, "error", "Missing 'windows' array in request");
        obs_data_set_array(response_data, "created", created_array);
        obs_data_set_array(response_data, "failed", failed_array);
        obs_data_array_release(created_array);
        obs_data_array_release(failed_array);
        return;
    }
    
    /* Get or create the CrowdCast scene */
    obs_source_t *scene_source = obs_get_source_by_name("CrowdCast Capture");
    obs_scene_t *scene = NULL;
    
    if (!scene_source) {
        /* Create the scene if it doesn't exist */
        scene = obs_scene_create("CrowdCast Capture");
        if (scene) {
            scene_source = obs_scene_get_source(scene);
            blog(LOG_INFO, "[crowdcast] Created 'CrowdCast Capture' scene");
        }
    } else {
        scene = obs_scene_from_source(scene_source);
    }
    
    if (!scene) {
        blog(LOG_ERROR, "[crowdcast] Failed to get or create CrowdCast scene");
        obs_data_set_bool(response_data, "success", false);
        obs_data_set_string(response_data, "error", "Failed to get or create scene");
        obs_data_set_array(response_data, "created", created_array);
        obs_data_set_array(response_data, "failed", failed_array);
        obs_data_array_release(created_array);
        obs_data_array_release(failed_array);
        if (scene_source) obs_source_release(scene_source);
        return;
    }
    
    size_t count = obs_data_array_count(windows);
    blog(LOG_INFO, "[crowdcast] Creating %zu capture sources", count);
    
    for (size_t i = 0; i < count; i++) {
        obs_data_t *window = obs_data_array_item(windows, i);
        const char *window_id = obs_data_get_string(window, "id");
        const char *source_name = obs_data_get_string(window, "name");
        
        if (!window_id || !source_name || strlen(window_id) == 0) {
            obs_data_release(window);
            continue;
        }
        
        /* Check if source already exists */
        obs_source_t *existing = obs_get_source_by_name(source_name);
        if (existing) {
            blog(LOG_INFO, "[crowdcast] Source '%s' already exists, skipping", source_name);
            obs_source_release(existing);
            obs_data_release(window);
            continue;
        }
        
        /* Create settings for the window capture source */
        obs_data_t *settings = obs_data_create();
        
        /* Platform-specific settings */
#ifdef _WIN32
        obs_data_set_string(settings, window_prop, window_id);
        obs_data_set_bool(settings, "cursor", true);
        obs_data_set_bool(settings, "compatibility", false);
#elif defined(__APPLE__)
        /* ScreenCaptureKit application capture settings */
        obs_data_set_int(settings, "type", get_capture_type());  /* 2 = Application capture */
        obs_data_set_string(settings, "application", window_id);  /* Bundle ID or app identifier */
        obs_data_set_bool(settings, "show_cursor", true);
        obs_data_set_bool(settings, "show_hidden_windows", false);
#else
        /* Linux */
        obs_data_set_string(settings, window_prop, window_id);
        obs_data_set_bool(settings, "cursor", true);
#endif
        
        /* Create the source */
        obs_source_t *new_source = obs_source_create(source_id, source_name, settings, NULL);
        obs_data_release(settings);
        
        if (new_source) {
            /* Add to scene */
            obs_scene_add(scene, new_source);
            obs_source_release(new_source);
            
            obs_data_t *created_obj = obs_data_create();
            obs_data_set_string(created_obj, "name", source_name);
            obs_data_set_string(created_obj, "id", window_id);
            obs_data_array_push_back(created_array, created_obj);
            obs_data_release(created_obj);
            
            success_count++;
            blog(LOG_INFO, "[crowdcast] Created source '%s'", source_name);
        } else {
            obs_data_t *failed_obj = obs_data_create();
            obs_data_set_string(failed_obj, "name", source_name);
            obs_data_set_string(failed_obj, "error", "Failed to create source");
            obs_data_array_push_back(failed_array, failed_obj);
            obs_data_release(failed_obj);
            
            fail_count++;
            blog(LOG_WARNING, "[crowdcast] Failed to create source '%s'", source_name);
        }
        
        obs_data_release(window);
    }
    
    obs_data_array_release(windows);
    if (scene_source) obs_source_release(scene_source);
    
    obs_data_set_bool(response_data, "success", fail_count == 0);
    obs_data_set_int(response_data, "created_count", success_count);
    obs_data_set_int(response_data, "failed_count", fail_count);
    obs_data_set_array(response_data, "created", created_array);
    obs_data_set_array(response_data, "failed", failed_array);
    
    obs_data_array_release(created_array);
    obs_data_array_release(failed_array);
    
    blog(LOG_INFO, "[crowdcast] CreateCaptureSources completed: %d created, %d failed",
         success_count, fail_count);
}

/* ========================================================================== */
/* Module Load/Unload                                                          */
/* ========================================================================== */

bool obs_module_load(void)
{
    blog(LOG_INFO, "[crowdcast] Plugin loading...");
    
    /* Initialize state */
    memset(g_sources, 0, sizeof(g_sources));
    g_source_count = 0;
    
    /* 
     * Note: Vendor registration happens in obs_module_post_load() because
     * obs-websocket's proc_handler is not available until after all modules
     * have been loaded via obs_module_load().
     */
    
    /* Register for global source create/remove signals */
    signal_handler_t *sh = obs_get_signal_handler();
    if (sh) {
        signal_handler_connect(sh, "source_create", on_source_created, NULL);
        signal_handler_connect(sh, "source_remove", on_source_removed, NULL);
    }
    
    /* Enumerate existing sources */
    enumerate_existing_sources();
    
    blog(LOG_INFO, "[crowdcast] Plugin loaded successfully");
    return true;
}

void obs_module_post_load(void)
{
    blog(LOG_INFO, "[crowdcast] Post-load: registering vendor requests...");
    
    /* 
     * Use the official obs-websocket API (proc_handler based).
     * This must be done in post_load because obs-websocket registers
     * its proc_handler in its own obs_module_load().
     */
    unsigned int api_version = obs_websocket_get_api_version();
    if (api_version == 0) {
        blog(LOG_WARNING, "[crowdcast] obs-websocket not available (API version 0)");
        return;
    }
    
    blog(LOG_INFO, "[crowdcast] obs-websocket API version: %u", api_version);
    
    g_vendor = obs_websocket_register_vendor("crowdcast");
    if (!g_vendor) {
        blog(LOG_WARNING, "[crowdcast] Failed to register vendor");
        return;
    }
    
    blog(LOG_INFO, "[crowdcast] Registered vendor 'crowdcast'");
    
    /* Register our vendor requests */
    bool ok1 = obs_websocket_vendor_register_request(g_vendor, "GetHookedSources", 
                                                     get_hooked_sources_cb, NULL);
    bool ok2 = obs_websocket_vendor_register_request(g_vendor, "GetAvailableWindows",
                                                     get_available_windows_cb, NULL);
    bool ok3 = obs_websocket_vendor_register_request(g_vendor, "CreateCaptureSources",
                                                     create_capture_sources_cb, NULL);
    
    if (ok1 && ok2 && ok3) {
        blog(LOG_INFO, "[crowdcast] Registered all vendor requests: "
                       "GetHookedSources, GetAvailableWindows, CreateCaptureSources");
    } else {
        blog(LOG_WARNING, "[crowdcast] Some vendor requests failed to register: "
                          "GetHookedSources=%d, GetAvailableWindows=%d, CreateCaptureSources=%d",
                          ok1, ok2, ok3);
    }
}

void obs_module_unload(void)
{
    blog(LOG_INFO, "[crowdcast] Plugin unloading...");
    
    /* Disconnect global signals */
    signal_handler_t *sh = obs_get_signal_handler();
    if (sh) {
        signal_handler_disconnect(sh, "source_create", on_source_created, NULL);
        signal_handler_disconnect(sh, "source_remove", on_source_removed, NULL);
    }
    
    /* Clear state */
    pthread_mutex_lock(&g_state_mutex);
    memset(g_sources, 0, sizeof(g_sources));
    g_source_count = 0;
    pthread_mutex_unlock(&g_state_mutex);
    
    blog(LOG_INFO, "[crowdcast] Plugin unloaded");
}
