/*
 * macOS notification support using UNUserNotificationCenter
 * Provides notifications with actionable buttons for display change alerts
 */

#import <Foundation/Foundation.h>
#import <UserNotifications/UserNotifications.h>
#include <stdint.h>

// Callback function pointer type for notification actions
typedef void (*NotificationActionCallback)(const char* action_id, uint32_t display_id);

// Static storage for the callback
static NotificationActionCallback g_action_callback = NULL;
static BOOL g_initialized = NO;

// Category identifiers
static NSString* const CATEGORY_DISPLAY_CHANGE = @"DISPLAY_CHANGE";

// Notification delegate to handle user responses
@interface CrowdCastNotificationDelegate : NSObject <UNUserNotificationCenterDelegate>
@end

@implementation CrowdCastNotificationDelegate

- (void)userNotificationCenter:(UNUserNotificationCenter *)center
       willPresentNotification:(UNNotification *)notification
         withCompletionHandler:(void (^)(UNNotificationPresentationOptions))completionHandler {
    // Show notification even when app is in foreground
    completionHandler(UNNotificationPresentationOptionBanner);
}

- (void)userNotificationCenter:(UNUserNotificationCenter *)center
didReceiveNotificationResponse:(UNNotificationResponse *)response
         withCompletionHandler:(void (^)(void))completionHandler {
    
    NSString *actionIdentifier = response.actionIdentifier;
    NSDictionary *userInfo = response.notification.request.content.userInfo;
    
    // Extract display_id from userInfo
    uint32_t displayId = 0;
    NSNumber *displayIdNum = userInfo[@"display_id"];
    if (displayIdNum) {
        displayId = [displayIdNum unsignedIntValue];
    }
    
    // Call the Rust callback if set (informational notifications - just track dismissal)
    if (g_action_callback) {
        if ([actionIdentifier isEqualToString:UNNotificationDefaultActionIdentifier]) {
            // User tapped the notification itself
            g_action_callback("default", displayId);
        } else if ([actionIdentifier isEqualToString:UNNotificationDismissActionIdentifier]) {
            // User dismissed the notification
            g_action_callback("dismiss", displayId);
        }
    }
    
    completionHandler();
}

@end

static CrowdCastNotificationDelegate *g_delegate = nil;

// Initialize the notification system and request permissions
// Returns: 0 on success, -1 on failure
int notifications_init(NotificationActionCallback callback) {
    if (g_initialized) {
        return 0;
    }
    
    g_action_callback = callback;
    
    @autoreleasepool {
        UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
        
        // Create and set delegate
        g_delegate = [[CrowdCastNotificationDelegate alloc] init];
        center.delegate = g_delegate;
        
        // Create category for display change notifications (informational, no action buttons)
        UNNotificationCategory *displayChangeCategory = [UNNotificationCategory
            categoryWithIdentifier:CATEGORY_DISPLAY_CHANGE
            actions:@[]  // No action buttons - auto-switch already happened
            intentIdentifiers:@[]
            options:UNNotificationCategoryOptionNone];
        
        // Register the category
        [center setNotificationCategories:[NSSet setWithObject:displayChangeCategory]];
        
        // Request authorization
        [center requestAuthorizationWithOptions:(UNAuthorizationOptionAlert | UNAuthorizationOptionSound)
                              completionHandler:^(BOOL granted, NSError * _Nullable error) {
            if (granted) {
                NSLog(@"[CrowdCast] Notification permission granted");
            } else {
                NSLog(@"[CrowdCast] Notification permission denied: %@", error);
            }
        }];
        
        g_initialized = YES;
    }
    
    return 0;
}

// Show a notification when display changes
// from_display: Name of the previous display
// to_display: Name of the new display  
// to_display_id: ID of the new display (passed back in action callback)
void notifications_show_display_change(const char* from_display, const char* to_display, uint32_t to_display_id) {
    if (!g_initialized) {
        NSLog(@"[CrowdCast] Notifications not initialized");
        return;
    }
    
    @autoreleasepool {
        UNMutableNotificationContent *content = [[UNMutableNotificationContent alloc] init];
        content.title = @"Display Changed";
        content.body = [NSString stringWithFormat:@"Now recording on %s (was %s).",
                        to_display, from_display];
        content.categoryIdentifier = CATEGORY_DISPLAY_CHANGE;
        content.userInfo = @{
            @"display_id": @(to_display_id),
            @"from_display": [NSString stringWithUTF8String:from_display],
            @"to_display": [NSString stringWithUTF8String:to_display]
        };
        
        // Create request with unique identifier
        NSString *identifier = [[NSUUID UUID] UUIDString];
        UNNotificationRequest *request = [UNNotificationRequest
            requestWithIdentifier:identifier
            content:content
            trigger:nil]; // Deliver immediately
        
        UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
        [center addNotificationRequest:request withCompletionHandler:^(NSError * _Nullable error) {
            if (error) {
                NSLog(@"[CrowdCast] Failed to show notification: %@", error);
            }
        }];
    }
}

// Show a notification when capture resumes
void notifications_show_capture_resumed(const char* display_name) {
    if (!g_initialized) {
        NSLog(@"[CrowdCast] Notifications not initialized");
        return;
    }
    
    @autoreleasepool {
        UNMutableNotificationContent *content = [[UNMutableNotificationContent alloc] init];
        content.title = @"Capture Resumed";
        content.body = [NSString stringWithFormat:@"Recording on %s", display_name];
        // Create request with unique identifier
        NSString *identifier = [[NSUUID UUID] UUIDString];
        UNNotificationRequest *request = [UNNotificationRequest
            requestWithIdentifier:identifier
            content:content
            trigger:nil]; // Deliver immediately
        
        UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
        [center addNotificationRequest:request withCompletionHandler:^(NSError * _Nullable error) {
            if (error) {
                NSLog(@"[CrowdCast] Failed to show notification: %@", error);
            }
        }];
    }
}

// Show a notification when recording starts
void notifications_show_recording_started(void) {
    if (!g_initialized) {
        NSLog(@"[CrowdCast] Notifications not initialized");
        return;
    }

    @autoreleasepool {
        UNMutableNotificationContent *content = [[UNMutableNotificationContent alloc] init];
        content.title = @"";
        content.body = @"Recording started";

        // Create request with unique identifier
        NSString *identifier = [[NSUUID UUID] UUIDString];
        UNNotificationRequest *request = [UNNotificationRequest
            requestWithIdentifier:identifier
            content:content
            trigger:nil]; // Deliver immediately

        UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
        [center addNotificationRequest:request withCompletionHandler:^(NSError * _Nullable error) {
            if (error) {
                NSLog(@"[CrowdCast] Failed to show notification: %@", error);
            }
        }];
    }
}

// Show a notification when recording stops
void notifications_show_recording_stopped(void) {
    if (!g_initialized) {
        NSLog(@"[CrowdCast] Notifications not initialized");
        return;
    }

    @autoreleasepool {
        UNMutableNotificationContent *content = [[UNMutableNotificationContent alloc] init];
        content.title = @"";
        content.body = @"Recording stopped";

        // Create request with unique identifier
        NSString *identifier = [[NSUUID UUID] UUIDString];
        UNNotificationRequest *request = [UNNotificationRequest
            requestWithIdentifier:identifier
            content:content
            trigger:nil]; // Deliver immediately

        UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
        [center addNotificationRequest:request withCompletionHandler:^(NSError * _Nullable error) {
            if (error) {
                NSLog(@"[CrowdCast] Failed to show notification: %@", error);
            }
        }];
    }
}

// Show a notification when recording is paused
void notifications_show_recording_paused(void) {
    if (!g_initialized) {
        NSLog(@"[CrowdCast] Notifications not initialized");
        return;
    }

    @autoreleasepool {
        UNMutableNotificationContent *content = [[UNMutableNotificationContent alloc] init];
        content.title = @"";
        content.body = @"Recording paused";

        // Create request with unique identifier
        NSString *identifier = [[NSUUID UUID] UUIDString];
        UNNotificationRequest *request = [UNNotificationRequest
            requestWithIdentifier:identifier
            content:content
            trigger:nil]; // Deliver immediately

        UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
        [center addNotificationRequest:request withCompletionHandler:^(NSError * _Nullable error) {
            if (error) {
                NSLog(@"[CrowdCast] Failed to show notification: %@", error);
            }
        }];
    }
}

// Show a notification when recording is resumed
void notifications_show_recording_resumed(void) {
    if (!g_initialized) {
        NSLog(@"[CrowdCast] Notifications not initialized");
        return;
    }

    @autoreleasepool {
        UNMutableNotificationContent *content = [[UNMutableNotificationContent alloc] init];
        content.title = @"";
        content.body = @"Recording resumed";

        // Create request with unique identifier
        NSString *identifier = [[NSUUID UUID] UUIDString];
        UNNotificationRequest *request = [UNNotificationRequest
            requestWithIdentifier:identifier
            content:content
            trigger:nil]; // Deliver immediately

        UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
        [center addNotificationRequest:request withCompletionHandler:^(NSError * _Nullable error) {
            if (error) {
                NSLog(@"[CrowdCast] Failed to show notification: %@", error);
            }
        }];
    }
}

// Show a notification when OBS download starts
void notifications_show_obs_download_started(void) {
    if (!g_initialized) {
        NSLog(@"[CrowdCast] Notifications not initialized");
        return;
    }

    @autoreleasepool {
        UNMutableNotificationContent *content = [[UNMutableNotificationContent alloc] init];
        content.title = @"Downloading OBS";
        content.body = @"Preparing capture components. This may take a minute.";
        // No sound for a lightweight notification

        // Create request with unique identifier
        NSString *identifier = [[NSUUID UUID] UUIDString];
        UNNotificationRequest *request = [UNNotificationRequest
            requestWithIdentifier:identifier
            content:content
            trigger:nil]; // Deliver immediately

        UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
        [center addNotificationRequest:request withCompletionHandler:^(NSError * _Nullable error) {
            if (error) {
                NSLog(@"[CrowdCast] Failed to show notification: %@", error);
            }
        }];
    }
}

// Show a notification after setup wizard finishes
void notifications_show_setup_configuring(void) {
    if (!g_initialized) {
        NSLog(@"[CrowdCast] Notifications not initialized");
        return;
    }

    @autoreleasepool {
        UNMutableNotificationContent *content = [[UNMutableNotificationContent alloc] init];
        content.title = @"Setting up Crowd-Cast";
        content.body = @"Configuring components in the background. OBS installation will start shortly.";
        // No sound for a lightweight notification

        // Create request with unique identifier
        NSString *identifier = [[NSUUID UUID] UUIDString];
        UNNotificationRequest *request = [UNNotificationRequest
            requestWithIdentifier:identifier
            content:content
            trigger:nil]; // Deliver immediately

        UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
        [center addNotificationRequest:request withCompletionHandler:^(NSError * _Nullable error) {
            if (error) {
                NSLog(@"[CrowdCast] Failed to show notification: %@", error);
            }
        }];
    }
}

// Show a notification when capture sources are refreshed
void notifications_show_sources_refreshed(void) {
    if (!g_initialized) {
        NSLog(@"[CrowdCast] Notifications not initialized");
        return;
    }

    @autoreleasepool {
        UNMutableNotificationContent *content = [[UNMutableNotificationContent alloc] init];
        content.title = @"Sources refreshed";
        content.body = @"Capture sources updated.";
        // No sound for a lightweight notification

        // Create request with unique identifier
        NSString *identifier = [[NSUUID UUID] UUIDString];
        UNNotificationRequest *request = [UNNotificationRequest
            requestWithIdentifier:identifier
            content:content
            trigger:nil]; // Deliver immediately

        UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
        [center addNotificationRequest:request withCompletionHandler:^(NSError * _Nullable error) {
            if (error) {
                NSLog(@"[CrowdCast] Failed to show notification: %@", error);
            }
        }];
    }
}

// Show a notification when recording is paused due to user inactivity
void notifications_show_idle_paused(void) {
    if (!g_initialized) {
        NSLog(@"[CrowdCast] Notifications not initialized");
        return;
    }

    @autoreleasepool {
        UNMutableNotificationContent *content = [[UNMutableNotificationContent alloc] init];
        content.title = @"";
        content.body = @"Recording paused (idle)";

        // Create request with unique identifier
        NSString *identifier = [[NSUUID UUID] UUIDString];
        UNNotificationRequest *request = [UNNotificationRequest
            requestWithIdentifier:identifier
            content:content
            trigger:nil]; // Deliver immediately

        UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
        [center addNotificationRequest:request withCompletionHandler:^(NSError * _Nullable error) {
            if (error) {
                NSLog(@"[CrowdCast] Failed to show notification: %@", error);
            }
        }];
    }
}

// Show a notification when recording resumes after user activity detected
void notifications_show_idle_resumed(void) {
    if (!g_initialized) {
        NSLog(@"[CrowdCast] Notifications not initialized");
        return;
    }

    @autoreleasepool {
        UNMutableNotificationContent *content = [[UNMutableNotificationContent alloc] init];
        content.title = @"";
        content.body = @"Recording resumed";

        // Create request with unique identifier
        NSString *identifier = [[NSUUID UUID] UUIDString];
        UNNotificationRequest *request = [UNNotificationRequest
            requestWithIdentifier:identifier
            content:content
            trigger:nil]; // Deliver immediately

        UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
        [center addNotificationRequest:request withCompletionHandler:^(NSError * _Nullable error) {
            if (error) {
                NSLog(@"[CrowdCast] Failed to show notification: %@", error);
            }
        }];
    }
}

// Check if notifications are authorized
// Returns: 1 if authorized, 0 if not, -1 on error
int notifications_is_authorized(void) {
    __block int result = -1;
    dispatch_semaphore_t semaphore = dispatch_semaphore_create(0);
    
    @autoreleasepool {
        UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
        [center getNotificationSettingsWithCompletionHandler:^(UNNotificationSettings * _Nonnull settings) {
            if (settings.authorizationStatus == UNAuthorizationStatusAuthorized) {
                result = 1;
            } else {
                result = 0;
            }
            dispatch_semaphore_signal(semaphore);
        }];
    }
    
    dispatch_semaphore_wait(semaphore, dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC));
    return result;
}
