/*
 * Platform-specific APIs for frontmost application detection.
 * 
 * Used to determine if any configured capture source's target application
 * is currently the focused/frontmost application on the system.
 */

#ifndef CROWDCAST_PLATFORM_H
#define CROWDCAST_PLATFORM_H

#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Get the identifier of the currently focused/frontmost application.
 * 
 * Returns:
 *   - macOS: Bundle identifier (e.g., "com.microsoft.VSCode")
 *   - Windows: Executable name (e.g., "Code.exe") 
 *   - Linux X11: WM_CLASS instance name (e.g., "code")
 *   - Linux Wayland: NULL (not supported)
 * 
 * The caller is responsible for freeing the returned string.
 * Returns NULL if the frontmost app cannot be determined.
 */
char *platform_get_frontmost_app_id(void);

/*
 * Check if we're running on Wayland (Linux only).
 * Returns true if Wayland session detected, false otherwise.
 * Always returns false on non-Linux platforms.
 */
bool platform_is_wayland(void);

/*
 * Check if the given frontmost app ID matches a target app ID from source settings.
 * 
 * This handles platform-specific matching logic:
 *   - macOS: Direct bundle ID comparison
 *   - Windows: Case-insensitive executable name comparison
 *   - Linux: Case-insensitive WM_CLASS comparison
 * 
 * Returns true if the IDs match (frontmost app is the capture target).
 */
bool platform_app_ids_match(const char *frontmost_id, const char *target_id);

#ifdef __cplusplus
}
#endif

#endif /* CROWDCAST_PLATFORM_H */
