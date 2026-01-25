/*
 * macOS platform implementation for frontmost application detection.
 * Uses NSWorkspace to get the currently focused application's bundle identifier.
 */

#import <AppKit/AppKit.h>
#include <stdlib.h>
#include <string.h>
#include "platform.h"

char *platform_get_frontmost_app_id(void)
{
    @autoreleasepool {
        NSRunningApplication *frontmost = [[NSWorkspace sharedWorkspace] frontmostApplication];
        if (!frontmost) {
            return NULL;
        }
        
        NSString *bundleId = frontmost.bundleIdentifier;
        if (!bundleId || bundleId.length == 0) {
            /* Fallback to localized name if no bundle ID */
            NSString *name = frontmost.localizedName;
            if (name && name.length > 0) {
                return strdup([name UTF8String]);
            }
            return NULL;
        }
        
        return strdup([bundleId UTF8String]);
    }
}

bool platform_is_wayland(void)
{
    /* macOS doesn't have Wayland */
    return false;
}

bool platform_app_ids_match(const char *frontmost_id, const char *target_id)
{
    if (!frontmost_id || !target_id) {
        return false;
    }
    
    /* 
     * On macOS, the target_id from screen_capture source settings is the bundle ID
     * (e.g., "com.microsoft.VSCode", "com.apple.Safari").
     * Direct comparison should work.
     */
    return strcmp(frontmost_id, target_id) == 0;
}
