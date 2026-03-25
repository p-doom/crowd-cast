/*
 * Tray icon library - macOS implementation
 * Based on dmikushin/tray (https://github.com/dmikushin/tray)
 * MIT License
 */

#import <Cocoa/Cocoa.h>
#include "tray.h"

static NSStatusItem *statusItem = nil;
static NSMenu *menu = nil;
static struct tray *currentTray = nil;
static BOOL shouldExit = NO;

@interface TrayDelegate : NSObject <NSMenuDelegate, NSApplicationDelegate>
@end

@implementation TrayDelegate

- (void)menuItemClicked:(id)sender {
    NSMenuItem *item = (NSMenuItem *)sender;
    NSValue *value = item.representedObject;
    if (value != nil) {
        struct tray_menu *m = (struct tray_menu *)[value pointerValue];
        if (m && m->cb) {
            m->cb(m);
        }
    }
}

- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)sender {
    return NSTerminateNow;
}

@end

static TrayDelegate *delegate = nil;

static NSMenu *_tray_menu(struct tray_menu *m) {
    NSMenu *menu = [[NSMenu alloc] init];
    [menu setAutoenablesItems:NO];
    
    for (; m != NULL && m->text != NULL; m++) {
        if (strcmp(m->text, "-") == 0) {
            [menu addItem:[NSMenuItem separatorItem]];
        } else {
            NSMenuItem *item = [[NSMenuItem alloc]
                initWithTitle:[NSString stringWithUTF8String:m->text]
                action:@selector(menuItemClicked:)
                keyEquivalent:@""];
            
            [item setTarget:delegate];
            // Wrap the pointer in NSValue to safely store it as representedObject
            NSValue *ptrValue = [NSValue valueWithPointer:m];
            [item setRepresentedObject:ptrValue];
            [item setEnabled:m->disabled ? NO : YES];
            [item setState:m->checked ? NSControlStateValueOn : NSControlStateValueOff];
            
            if (m->submenu != NULL) {
                [item setSubmenu:_tray_menu(m->submenu)];
            }
            
            [menu addItem:item];
        }
    }
    
    return menu;
}

int tray_init(struct tray *tray) {
    @autoreleasepool {
        NSApplication *app = [NSApplication sharedApplication];
        [app setActivationPolicy:NSApplicationActivationPolicyAccessory];

        delegate = [[TrayDelegate alloc] init];
        [app setDelegate:delegate];

        // Briefly start [NSApp run] and immediately stop it.
        // [NSApp run] registers the Apple Event Mach port on the main run
        // loop and installs default AE handlers (including kAEQuitApplication).
        // Our custom event loop bypasses [NSApp run], so without this the
        // Sparkle Updater's [NSRunningApplication terminate] Apple Event goes
        // unhandled (-1708) and the app never quits for updates.
        dispatch_async(dispatch_get_main_queue(), ^{
            [NSApp stop:nil];
            [NSApp postEvent:[NSEvent otherEventWithType:NSEventTypeApplicationDefined
                                                location:NSZeroPoint
                                           modifierFlags:0
                                               timestamp:0
                                            windowNumber:0
                                                 context:nil
                                                 subtype:0
                                                   data1:0
                                                   data2:0]
                     atStart:YES];
        });
        [NSApp run];

        // Must be called on main thread - dispatch if needed
        if ([NSThread isMainThread]) {
            statusItem = [[NSStatusBar systemStatusBar]
                statusItemWithLength:NSVariableStatusItemLength];
        } else {
            dispatch_sync(dispatch_get_main_queue(), ^{
                statusItem = [[NSStatusBar systemStatusBar]
                    statusItemWithLength:NSVariableStatusItemLength];
            });
        }
        
        if (statusItem == nil) {
            return -1;
        }
        
        currentTray = tray;
        shouldExit = NO;
        
        tray_update(tray);
        
        return 0;
    }
}

int tray_loop(int blocking) {
    @autoreleasepool {
        @try {
            if (shouldExit) {
                return -1;
            }
            
            // Process pending events without blocking
            NSApplication *app = [NSApplication sharedApplication];
            NSEvent *event;
            while ((event = [app nextEventMatchingMask:NSEventMaskAny
                                            untilDate:blocking ? [NSDate distantFuture] : [NSDate distantPast]
                                               inMode:NSDefaultRunLoopMode
                                              dequeue:YES])) {
                [app sendEvent:event];

                // Check if we should exit after each event
                if (shouldExit) {
                    return -1;
                }
            }

            // Drain the main GCD queue and run loop sources.
            // Sparkle's XPC installer dispatches [NSApp terminate:] to the
            // main queue — nextEventMatchingMask: alone won't process it.
            [[NSRunLoop currentRunLoop] runMode:NSDefaultRunLoopMode
                                     beforeDate:[NSDate distantPast]];
            
            return shouldExit ? -1 : 0;
        } @catch (NSException *exception) {
            // Log exception but don't crash
            NSLog(@"Tray loop exception: %@", exception);
            return 0;
        }
    }
}

void tray_update(struct tray *tray) {
    void (^updateBlock)(void) = ^{
        @autoreleasepool {
            @try {
                currentTray = tray;
                
                if (statusItem == nil) {
                    return;
                }
                
                // Update icon
                if (tray->icon_filepath != NULL) {
                    NSString *path = [NSString stringWithUTF8String:tray->icon_filepath];
                    NSImage *image = [[NSImage alloc] initWithContentsOfFile:path];
                    if (image != nil) {
                        [image setSize:NSMakeSize(18, 18)];
                        [image setTemplate:NO];
                        statusItem.button.image = image;
                    }
                }
                
                // Update tooltip
                if (tray->tooltip != NULL) {
                    statusItem.button.toolTip = [NSString stringWithUTF8String:tray->tooltip];
                }
                
                // Update menu
                if (tray->menu != NULL) {
                    menu = _tray_menu(tray->menu);
                    statusItem.menu = menu;
                }
            } @catch (NSException *exception) {
                NSLog(@"Tray update exception: %@", exception);
            }
        }
    };
    
    // UI updates must happen on main thread
    if ([NSThread isMainThread]) {
        updateBlock();
    } else {
        dispatch_async(dispatch_get_main_queue(), updateBlock);
    }
}

void tray_exit(void) {
    shouldExit = YES;
}
