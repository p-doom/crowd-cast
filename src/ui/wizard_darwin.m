/*
 * Native macOS Setup Wizard
 * Provides a native Cocoa UI for the first-run setup experience
 */

#import <Cocoa/Cocoa.h>
#import <ApplicationServices/ApplicationServices.h>
#import <CoreGraphics/CoreGraphics.h>
#import <UserNotifications/UserNotifications.h>
#include "wizard_darwin.h"

// ============================================================================
// Global State
// ============================================================================

static NSMutableArray<NSDictionary *> *g_available_apps = nil;
static NSMutableSet<NSString *> *g_selected_apps = nil;
static WizardConfig *g_config = nil;
static BOOL g_wizard_running = NO;

// ============================================================================
// Wizard Window Controller
// ============================================================================

@interface WizardWindowController : NSWindowController <NSTableViewDataSource, NSTableViewDelegate>

@property (nonatomic) NSInteger currentStep;
@property (nonatomic, strong) NSView *contentView;
@property (nonatomic, strong) NSButton *backButton;
@property (nonatomic, strong) NSButton *nextButton;
@property (nonatomic, strong) NSButton *cancelButton;
@property (nonatomic, strong) NSProgressIndicator *stepIndicator;
@property (nonatomic, strong) NSTextField *stepLabel;

// Step views
@property (nonatomic, strong) NSView *welcomeView;
@property (nonatomic, strong) NSView *permissionsView;
@property (nonatomic, strong) NSView *appSelectionView;
@property (nonatomic, strong) NSView *autostartView;
@property (nonatomic, strong) NSView *summaryView;

// Permission status labels
@property (nonatomic, strong) NSTextField *screenRecordingStatus;
@property (nonatomic, strong) NSTextField *accessibilityStatus;
@property (nonatomic, strong) NSTextField *notificationsStatus;
@property (nonatomic, strong) NSTimer *permissionTimer;
@property (nonatomic) BOOL allPermissionsGranted;

// App selection
@property (nonatomic, strong) NSTableView *appTableView;
@property (nonatomic, strong) NSButton *captureAllCheckbox;

// Autostart
@property (nonatomic, strong) NSButton *autostartCheckbox;

@end

@implementation WizardWindowController

- (instancetype)init {
    NSRect frame = NSMakeRect(0, 0, 550, 450);
    NSWindow *window = [[NSWindow alloc]
        initWithContentRect:frame
        styleMask:(NSWindowStyleMaskTitled | NSWindowStyleMaskClosable)
        backing:NSBackingStoreBuffered
        defer:NO];
    
    self = [super initWithWindow:window];
    if (self) {
        _currentStep = 0;
        
        [window setTitle:@"crowd-cast Setup"];
        [window center];
        [window setReleasedWhenClosed:NO];
        
        [self setupUI];
        [self showStep:0];
    }
    return self;
}

- (void)setupUI {
    NSView *mainView = self.window.contentView;
    
    // Content area (top portion)
    _contentView = [[NSView alloc] initWithFrame:NSMakeRect(0, 60, 550, 390)];
    [mainView addSubview:_contentView];
    
    // Bottom bar with buttons
    NSView *bottomBar = [[NSView alloc] initWithFrame:NSMakeRect(0, 0, 550, 60)];
    bottomBar.wantsLayer = YES;
    bottomBar.layer.backgroundColor = [[NSColor colorWithWhite:0.95 alpha:1.0] CGColor];
    [mainView addSubview:bottomBar];
    
    // Separator line
    NSBox *separator = [[NSBox alloc] initWithFrame:NSMakeRect(0, 59, 550, 1)];
    separator.boxType = NSBoxSeparator;
    [mainView addSubview:separator];
    
    // Cancel button (left)
    _cancelButton = [[NSButton alloc] initWithFrame:NSMakeRect(20, 15, 80, 30)];
    _cancelButton.bezelStyle = NSBezelStyleRounded;
    _cancelButton.title = @"Cancel";
    _cancelButton.target = self;
    _cancelButton.action = @selector(cancelClicked:);
    [bottomBar addSubview:_cancelButton];
    
    // Next button (right)
    _nextButton = [[NSButton alloc] initWithFrame:NSMakeRect(450, 15, 80, 30)];
    _nextButton.bezelStyle = NSBezelStyleRounded;
    _nextButton.title = @"Next";
    _nextButton.keyEquivalent = @"\r";
    _nextButton.target = self;
    _nextButton.action = @selector(nextClicked:);
    [bottomBar addSubview:_nextButton];
    
    // Back button
    _backButton = [[NSButton alloc] initWithFrame:NSMakeRect(360, 15, 80, 30)];
    _backButton.bezelStyle = NSBezelStyleRounded;
    _backButton.title = @"Back";
    _backButton.target = self;
    _backButton.action = @selector(backClicked:);
    [bottomBar addSubview:_backButton];
    
    // Step indicator label
    _stepLabel = [[NSTextField alloc] initWithFrame:NSMakeRect(150, 20, 200, 20)];
    _stepLabel.bezeled = NO;
    _stepLabel.editable = NO;
    _stepLabel.drawsBackground = NO;
    _stepLabel.alignment = NSTextAlignmentCenter;
    _stepLabel.textColor = [NSColor secondaryLabelColor];
    _stepLabel.font = [NSFont systemFontOfSize:12];
    [bottomBar addSubview:_stepLabel];
    
    // Create step views
    [self createWelcomeView];
    [self createPermissionsView];
    [self createAppSelectionView];
    [self createAutostartView];
    [self createSummaryView];
}

// ============================================================================
// Welcome Step
// ============================================================================

- (void)createWelcomeView {
    _welcomeView = [[NSView alloc] initWithFrame:_contentView.bounds];
    
    // Title
    NSTextField *title = [[NSTextField alloc] initWithFrame:NSMakeRect(0, 300, 550, 40)];
    title.stringValue = @"Welcome to crowd-cast";
    title.bezeled = NO;
    title.editable = NO;
    title.drawsBackground = NO;
    title.alignment = NSTextAlignmentCenter;
    title.font = [NSFont boldSystemFontOfSize:24];
    [_welcomeView addSubview:title];
    
    // Description
    NSTextField *desc = [[NSTextField alloc] initWithFrame:NSMakeRect(50, 200, 450, 80)];
    desc.stringValue = @"crowd-cast captures your screen and input data to help build better AI coding assistants.\n\nThis wizard will help you configure the app.";
    desc.bezeled = NO;
    desc.editable = NO;
    desc.drawsBackground = NO;
    desc.alignment = NSTextAlignmentCenter;
    desc.font = [NSFont systemFontOfSize:14];
    desc.textColor = [NSColor secondaryLabelColor];
    [desc setLineBreakMode:NSLineBreakByWordWrapping];
    [_welcomeView addSubview:desc];
    
    // Setup steps
    NSTextField *steps = [[NSTextField alloc] initWithFrame:NSMakeRect(100, 80, 350, 100)];
    steps.stringValue = @"Setup will:\n\n  1. Request necessary permissions\n  2. Let you select applications to capture\n  3. Configure automatic startup";
    steps.bezeled = NO;
    steps.editable = NO;
    steps.drawsBackground = NO;
    steps.font = [NSFont systemFontOfSize:13];
    steps.textColor = [NSColor labelColor];
    [_welcomeView addSubview:steps];
}

// ============================================================================
// Permissions Step
// ============================================================================

- (void)createPermissionsView {
    _permissionsView = [[NSView alloc] initWithFrame:_contentView.bounds];
    
    // Title
    NSTextField *title = [[NSTextField alloc] initWithFrame:NSMakeRect(0, 340, 550, 30)];
    title.stringValue = @"Permissions";
    title.bezeled = NO;
    title.editable = NO;
    title.drawsBackground = NO;
    title.alignment = NSTextAlignmentCenter;
    title.font = [NSFont boldSystemFontOfSize:20];
    [_permissionsView addSubview:title];
    
    // Description
    NSTextField *desc = [[NSTextField alloc] initWithFrame:NSMakeRect(50, 300, 450, 35)];
    desc.stringValue = @"crowd-cast needs the following permissions:";
    desc.bezeled = NO;
    desc.editable = NO;
    desc.drawsBackground = NO;
    desc.alignment = NSTextAlignmentCenter;
    desc.font = [NSFont systemFontOfSize:13];
    desc.textColor = [NSColor secondaryLabelColor];
    [_permissionsView addSubview:desc];
    
    // Screen Recording permission row (y=230)
    _screenRecordingStatus = [self createPermissionRowWithName:@"Screen Recording"
                  description:@"Required to capture your screen"
                            y:230
                 grantAction:@selector(grantScreenRecording:)
                settingsAction:@selector(openScreenRecordingSettings:)];
    
    // Accessibility permission row (y=160)
    _accessibilityStatus = [self createPermissionRowWithName:@"Accessibility"
                  description:@"Required to capture keyboard and mouse"
                            y:160
                 grantAction:@selector(grantAccessibility:)
                settingsAction:@selector(openAccessibilitySettings:)];
    
    // Notifications permission row (y=90)
    _notificationsStatus = [self createPermissionRowWithName:@"Notifications"
                  description:@"For status updates and alerts"
                            y:90
                 grantAction:@selector(grantNotifications:)
                settingsAction:@selector(openNotificationsSettings:)];
    
    // Note
    NSTextField *note = [[NSTextField alloc] initWithFrame:NSMakeRect(50, 10, 450, 70)];
    note.stringValue = @"After granting permissions in System Settings, click \"Restart App\" to apply changes. Screen Recording and Accessibility require an app restart to take effect.";
    note.bezeled = NO;
    note.editable = NO;
    note.drawsBackground = NO;
    note.alignment = NSTextAlignmentCenter;
    note.font = [NSFont systemFontOfSize:11];
    note.textColor = [NSColor tertiaryLabelColor];
    [note setLineBreakMode:NSLineBreakByWordWrapping];
    [_permissionsView addSubview:note];
}

- (NSTextField *)createPermissionRowWithName:(NSString *)name
                description:(NSString *)desc
                          y:(CGFloat)y
               grantAction:(SEL)grantAction
            settingsAction:(SEL)settingsAction {
    
    // Background box
    NSBox *box = [[NSBox alloc] initWithFrame:NSMakeRect(40, y, 470, 60)];
    box.boxType = NSBoxCustom;
    box.fillColor = [NSColor colorWithWhite:0.97 alpha:1.0];
    box.borderColor = [NSColor colorWithWhite:0.9 alpha:1.0];
    box.borderWidth = 1;
    box.cornerRadius = 8;
    box.titlePosition = NSNoTitle;
    [_permissionsView addSubview:box];
    
    // Name label
    NSTextField *nameLabel = [[NSTextField alloc] initWithFrame:NSMakeRect(55, y + 32, 150, 20)];
    nameLabel.stringValue = name;
    nameLabel.bezeled = NO;
    nameLabel.editable = NO;
    nameLabel.drawsBackground = NO;
    nameLabel.font = [NSFont boldSystemFontOfSize:13];
    [_permissionsView addSubview:nameLabel];
    
    // Description label
    NSTextField *descLabel = [[NSTextField alloc] initWithFrame:NSMakeRect(55, y + 12, 250, 18)];
    descLabel.stringValue = desc;
    descLabel.bezeled = NO;
    descLabel.editable = NO;
    descLabel.drawsBackground = NO;
    descLabel.font = [NSFont systemFontOfSize:11];
    descLabel.textColor = [NSColor secondaryLabelColor];
    [_permissionsView addSubview:descLabel];
    
    // Status label
    NSTextField *statusLabel = [[NSTextField alloc] initWithFrame:NSMakeRect(300, y + 32, 80, 20)];
    statusLabel.bezeled = NO;
    statusLabel.editable = NO;
    statusLabel.drawsBackground = NO;
    statusLabel.alignment = NSTextAlignmentRight;
    statusLabel.font = [NSFont systemFontOfSize:12];
    [_permissionsView addSubview:statusLabel];
    
    // Grant button
    NSButton *grantBtn = [[NSButton alloc] initWithFrame:NSMakeRect(385, y + 15, 55, 28)];
    grantBtn.bezelStyle = NSBezelStyleRounded;
    grantBtn.title = @"Grant";
    grantBtn.font = [NSFont systemFontOfSize:11];
    grantBtn.target = self;
    grantBtn.action = grantAction;
    [_permissionsView addSubview:grantBtn];
    
    // Settings button
    NSButton *settingsBtn = [[NSButton alloc] initWithFrame:NSMakeRect(445, y + 15, 55, 28)];
    settingsBtn.bezelStyle = NSBezelStyleRounded;
    settingsBtn.title = @"Open";
    settingsBtn.font = [NSFont systemFontOfSize:11];
    settingsBtn.target = self;
    settingsBtn.action = settingsAction;
    [_permissionsView addSubview:settingsBtn];
    
    return statusLabel;
}

- (void)updatePermissionStatus {
    BOOL screenRecording = wizard_check_screen_recording();
    BOOL accessibility = wizard_check_accessibility();
    BOOL notifications = wizard_check_notifications();
    
    if (screenRecording) {
        _screenRecordingStatus.stringValue = @"Granted";
        _screenRecordingStatus.textColor = [NSColor systemGreenColor];
    } else {
        _screenRecordingStatus.stringValue = @"Required";
        _screenRecordingStatus.textColor = [NSColor systemOrangeColor];
    }
    
    if (accessibility) {
        _accessibilityStatus.stringValue = @"Granted";
        _accessibilityStatus.textColor = [NSColor systemGreenColor];
    } else {
        _accessibilityStatus.stringValue = @"Required";
        _accessibilityStatus.textColor = [NSColor systemOrangeColor];
    }
    
    if (notifications) {
        _notificationsStatus.stringValue = @"Granted";
        _notificationsStatus.textColor = [NSColor systemGreenColor];
    } else {
        _notificationsStatus.stringValue = @"Optional";
        _notificationsStatus.textColor = [NSColor systemOrangeColor];
    }
    
    // Update button state - Screen Recording and Accessibility are required
    BOOL requiredGranted = screenRecording && accessibility;
    _allPermissionsGranted = requiredGranted;
    
    // Update Next button on permissions step
    if (_currentStep == 1) {
        [self updateNextButtonForPermissions];
    }
}

- (void)updateNextButtonForPermissions {
    if (_allPermissionsGranted) {
        _nextButton.title = @"Next";
        _nextButton.bezelStyle = NSBezelStyleRounded;
    } else {
        _nextButton.title = @"Restart App";
        _nextButton.bezelStyle = NSBezelStyleRounded;
        // Make it more prominent with blue color
        if (@available(macOS 11.0, *)) {
            _nextButton.hasDestructiveAction = NO;
            _nextButton.bezelColor = [NSColor systemBlueColor];
        }
    }
}

- (void)grantScreenRecording:(id)sender {
    wizard_request_screen_recording();
    [self updatePermissionStatus];
}

- (void)grantAccessibility:(id)sender {
    wizard_request_accessibility();
    [self updatePermissionStatus];
}

- (void)grantNotifications:(id)sender {
    wizard_request_notifications();
    // Notifications request is async, update status after a short delay
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)(0.5 * NSEC_PER_SEC)), dispatch_get_main_queue(), ^{
        [self updatePermissionStatus];
    });
}

- (void)openScreenRecordingSettings:(id)sender {
    wizard_open_screen_recording_settings();
}

- (void)openAccessibilitySettings:(id)sender {
    wizard_open_accessibility_settings();
}

- (void)openNotificationsSettings:(id)sender {
    wizard_open_notifications_settings();
}

- (void)restartApp {
    // Get the path to the current app bundle
    NSString *appPath = [[NSBundle mainBundle] bundlePath];
    
    // Use NSTask to relaunch after a short delay
    NSTask *task = [[NSTask alloc] init];
    task.launchPath = @"/bin/sh";
    task.arguments = @[@"-c", [NSString stringWithFormat:@"sleep 0.5 && open '%@'", appPath]];
    [task launch];
    
    // Quit the current app
    [NSApp terminate:nil];
}

- (void)startPermissionTimer {
    [self stopPermissionTimer];
    _permissionTimer = [NSTimer scheduledTimerWithTimeInterval:1.0
                                                        target:self
                                                      selector:@selector(updatePermissionStatus)
                                                      userInfo:nil
                                                       repeats:YES];
    [self updatePermissionStatus];
}

- (void)stopPermissionTimer {
    if (_permissionTimer) {
        [_permissionTimer invalidate];
        _permissionTimer = nil;
    }
}

// ============================================================================
// App Selection Step
// ============================================================================

- (void)createAppSelectionView {
    _appSelectionView = [[NSView alloc] initWithFrame:_contentView.bounds];
    
    // Title
    NSTextField *title = [[NSTextField alloc] initWithFrame:NSMakeRect(0, 320, 550, 30)];
    title.stringValue = @"Select Applications";
    title.bezeled = NO;
    title.editable = NO;
    title.drawsBackground = NO;
    title.alignment = NSTextAlignmentCenter;
    title.font = [NSFont boldSystemFontOfSize:20];
    [_appSelectionView addSubview:title];
    
    // Description
    NSTextField *desc = [[NSTextField alloc] initWithFrame:NSMakeRect(50, 280, 450, 35)];
    desc.stringValue = @"Choose which applications to capture. Input will only be recorded when a selected app is active.";
    desc.bezeled = NO;
    desc.editable = NO;
    desc.drawsBackground = NO;
    desc.alignment = NSTextAlignmentCenter;
    desc.font = [NSFont systemFontOfSize:13];
    desc.textColor = [NSColor secondaryLabelColor];
    [desc setLineBreakMode:NSLineBreakByWordWrapping];
    [_appSelectionView addSubview:desc];
    
    // Capture all checkbox
    _captureAllCheckbox = [[NSButton alloc] initWithFrame:NSMakeRect(50, 245, 450, 22)];
    _captureAllCheckbox.buttonType = NSButtonTypeSwitch;
    _captureAllCheckbox.title = @"Capture all applications";
    _captureAllCheckbox.font = [NSFont boldSystemFontOfSize:13];
    _captureAllCheckbox.target = self;
    _captureAllCheckbox.action = @selector(captureAllChanged:);
    [_appSelectionView addSubview:_captureAllCheckbox];
    
    // Scroll view for app table
    NSScrollView *scrollView = [[NSScrollView alloc] initWithFrame:NSMakeRect(50, 30, 450, 200)];
    scrollView.hasVerticalScroller = YES;
    scrollView.hasHorizontalScroller = NO;
    scrollView.autohidesScrollers = YES;
    scrollView.borderType = NSBezelBorder;
    
    // Table view
    _appTableView = [[NSTableView alloc] initWithFrame:scrollView.bounds];
    _appTableView.dataSource = self;
    _appTableView.delegate = self;
    _appTableView.rowHeight = 28;
    _appTableView.allowsMultipleSelection = NO;
    _appTableView.headerView = nil;
    
    // Checkbox column
    NSTableColumn *checkColumn = [[NSTableColumn alloc] initWithIdentifier:@"check"];
    checkColumn.width = 30;
    checkColumn.minWidth = 30;
    checkColumn.maxWidth = 30;
    [_appTableView addTableColumn:checkColumn];
    
    // Name column
    NSTableColumn *nameColumn = [[NSTableColumn alloc] initWithIdentifier:@"name"];
    nameColumn.width = 200;
    nameColumn.title = @"Application";
    [_appTableView addTableColumn:nameColumn];
    
    // Bundle ID column
    NSTableColumn *bundleColumn = [[NSTableColumn alloc] initWithIdentifier:@"bundle"];
    bundleColumn.width = 200;
    bundleColumn.title = @"Bundle ID";
    [_appTableView addTableColumn:bundleColumn];
    
    scrollView.documentView = _appTableView;
    [_appSelectionView addSubview:scrollView];
}

- (void)captureAllChanged:(id)sender {
    BOOL enabled = (_captureAllCheckbox.state == NSControlStateValueOn);
    _appTableView.enabled = !enabled;
    if (enabled) {
        _appTableView.alphaValue = 0.5;
    } else {
        _appTableView.alphaValue = 1.0;
    }
}

// NSTableViewDataSource
- (NSInteger)numberOfRowsInTableView:(NSTableView *)tableView {
    return g_available_apps ? g_available_apps.count : 0;
}

// NSTableViewDelegate
- (NSView *)tableView:(NSTableView *)tableView viewForTableColumn:(NSTableColumn *)tableColumn row:(NSInteger)row {
    if (row >= (NSInteger)g_available_apps.count) return nil;
    
    NSDictionary *app = g_available_apps[row];
    NSString *identifier = tableColumn.identifier;
    
    if ([identifier isEqualToString:@"check"]) {
        NSButton *checkbox = [[NSButton alloc] initWithFrame:NSMakeRect(0, 0, 20, 20)];
        checkbox.buttonType = NSButtonTypeSwitch;
        checkbox.title = @"";
        checkbox.tag = row;
        checkbox.target = self;
        checkbox.action = @selector(appCheckboxChanged:);
        
        NSString *bundleId = app[@"bundle_id"];
        checkbox.state = [g_selected_apps containsObject:bundleId] ? NSControlStateValueOn : NSControlStateValueOff;
        
        return checkbox;
    } else if ([identifier isEqualToString:@"name"]) {
        NSTextField *field = [[NSTextField alloc] initWithFrame:NSMakeRect(0, 0, 200, 20)];
        field.stringValue = app[@"name"] ?: @"Unknown";
        field.bezeled = NO;
        field.editable = NO;
        field.drawsBackground = NO;
        field.font = [NSFont systemFontOfSize:12];
        return field;
    } else if ([identifier isEqualToString:@"bundle"]) {
        NSTextField *field = [[NSTextField alloc] initWithFrame:NSMakeRect(0, 0, 200, 20)];
        field.stringValue = app[@"bundle_id"] ?: @"";
        field.bezeled = NO;
        field.editable = NO;
        field.drawsBackground = NO;
        field.font = [NSFont systemFontOfSize:11];
        field.textColor = [NSColor secondaryLabelColor];
        return field;
    }
    
    return nil;
}

- (void)appCheckboxChanged:(id)sender {
    NSButton *checkbox = (NSButton *)sender;
    NSInteger row = checkbox.tag;
    
    if (row < (NSInteger)g_available_apps.count) {
        NSDictionary *app = g_available_apps[row];
        NSString *bundleId = app[@"bundle_id"];
        
        if (checkbox.state == NSControlStateValueOn) {
            [g_selected_apps addObject:bundleId];
        } else {
            [g_selected_apps removeObject:bundleId];
        }
    }
}

// ============================================================================
// Autostart Step
// ============================================================================

- (void)createAutostartView {
    _autostartView = [[NSView alloc] initWithFrame:_contentView.bounds];
    
    // Title
    NSTextField *title = [[NSTextField alloc] initWithFrame:NSMakeRect(0, 320, 550, 30)];
    title.stringValue = @"Automatic Startup";
    title.bezeled = NO;
    title.editable = NO;
    title.drawsBackground = NO;
    title.alignment = NSTextAlignmentCenter;
    title.font = [NSFont boldSystemFontOfSize:20];
    [_autostartView addSubview:title];
    
    // Description
    NSTextField *desc = [[NSTextField alloc] initWithFrame:NSMakeRect(50, 260, 450, 50)];
    desc.stringValue = @"Would you like crowd-cast to start automatically when you log in?";
    desc.bezeled = NO;
    desc.editable = NO;
    desc.drawsBackground = NO;
    desc.alignment = NSTextAlignmentCenter;
    desc.font = [NSFont systemFontOfSize:14];
    desc.textColor = [NSColor secondaryLabelColor];
    [_autostartView addSubview:desc];
    
    // Autostart checkbox in a box
    NSBox *box = [[NSBox alloc] initWithFrame:NSMakeRect(100, 150, 350, 80)];
    box.boxType = NSBoxCustom;
    box.fillColor = [NSColor colorWithWhite:0.97 alpha:1.0];
    box.borderColor = [NSColor colorWithWhite:0.9 alpha:1.0];
    box.borderWidth = 1;
    box.cornerRadius = 8;
    box.titlePosition = NSNoTitle;
    [_autostartView addSubview:box];
    
    _autostartCheckbox = [[NSButton alloc] initWithFrame:NSMakeRect(120, 175, 310, 22)];
    _autostartCheckbox.buttonType = NSButtonTypeSwitch;
    _autostartCheckbox.title = @"Start crowd-cast on login";
    _autostartCheckbox.font = [NSFont boldSystemFontOfSize:13];
    _autostartCheckbox.state = NSControlStateValueOn; // Default to on
    [_autostartView addSubview:_autostartCheckbox];
    
    NSTextField *autostartDesc = [[NSTextField alloc] initWithFrame:NSMakeRect(140, 155, 280, 18)];
    autostartDesc.stringValue = @"Recommended for continuous data collection";
    autostartDesc.bezeled = NO;
    autostartDesc.editable = NO;
    autostartDesc.drawsBackground = NO;
    autostartDesc.font = [NSFont systemFontOfSize:11];
    autostartDesc.textColor = [NSColor secondaryLabelColor];
    [_autostartView addSubview:autostartDesc];
}

// ============================================================================
// Summary Step
// ============================================================================

- (void)createSummaryView {
    _summaryView = [[NSView alloc] initWithFrame:_contentView.bounds];
    
    // Title
    NSTextField *title = [[NSTextField alloc] initWithFrame:NSMakeRect(0, 320, 550, 30)];
    title.stringValue = @"Setup Complete";
    title.bezeled = NO;
    title.editable = NO;
    title.drawsBackground = NO;
    title.alignment = NSTextAlignmentCenter;
    title.font = [NSFont boldSystemFontOfSize:20];
    [_summaryView addSubview:title];
    
    // Description
    NSTextField *desc = [[NSTextField alloc] initWithFrame:NSMakeRect(50, 280, 450, 30)];
    desc.stringValue = @"Here's a summary of your configuration:";
    desc.bezeled = NO;
    desc.editable = NO;
    desc.drawsBackground = NO;
    desc.alignment = NSTextAlignmentCenter;
    desc.font = [NSFont systemFontOfSize:13];
    desc.textColor = [NSColor secondaryLabelColor];
    [_summaryView addSubview:desc];
    
    // Summary box (will be populated when shown)
    NSBox *box = [[NSBox alloc] initWithFrame:NSMakeRect(80, 80, 390, 180)];
    box.boxType = NSBoxCustom;
    box.fillColor = [NSColor colorWithWhite:0.97 alpha:1.0];
    box.borderColor = [NSColor colorWithWhite:0.9 alpha:1.0];
    box.borderWidth = 1;
    box.cornerRadius = 8;
    box.titlePosition = NSNoTitle;
    [_summaryView addSubview:box];
}

- (void)updateSummaryView {
    // Remove old summary labels
    for (NSView *subview in [_summaryView.subviews copy]) {
        if (subview.tag >= 200 && subview.tag < 300) {
            [subview removeFromSuperview];
        }
    }
    
    CGFloat y = 220;
    
    // Permissions
    NSTextField *permLabel = [self createSummaryLabel:@"Permissions:" y:y bold:YES];
    permLabel.tag = 200;
    [_summaryView addSubview:permLabel];
    
    BOOL allGranted = wizard_check_screen_recording() && wizard_check_accessibility();
    NSTextField *permValue = [self createSummaryValue:allGranted ? @"All granted" : @"Some missing" y:y];
    permValue.textColor = allGranted ? [NSColor systemGreenColor] : [NSColor systemOrangeColor];
    permValue.tag = 201;
    [_summaryView addSubview:permValue];
    
    y -= 30;
    
    // Capture mode
    NSTextField *captureLabel = [self createSummaryLabel:@"Capture Mode:" y:y bold:YES];
    captureLabel.tag = 202;
    [_summaryView addSubview:captureLabel];
    
    NSString *captureValue;
    if (_captureAllCheckbox.state == NSControlStateValueOn) {
        captureValue = @"All applications";
    } else if (g_selected_apps.count == 0) {
        captureValue = @"No apps selected";
    } else {
        captureValue = [NSString stringWithFormat:@"%lu application(s)", (unsigned long)g_selected_apps.count];
    }
    NSTextField *captureValueField = [self createSummaryValue:captureValue y:y];
    captureValueField.tag = 203;
    [_summaryView addSubview:captureValueField];
    
    y -= 30;
    
    // Autostart
    NSTextField *autostartLabel = [self createSummaryLabel:@"Start on Login:" y:y bold:YES];
    autostartLabel.tag = 204;
    [_summaryView addSubview:autostartLabel];
    
    NSTextField *autostartValue = [self createSummaryValue:_autostartCheckbox.state == NSControlStateValueOn ? @"Yes" : @"No" y:y];
    autostartValue.tag = 205;
    [_summaryView addSubview:autostartValue];
    
    y -= 30;
    
    // Selected apps list (if not capture all)
    if (_captureAllCheckbox.state != NSControlStateValueOn && g_selected_apps.count > 0) {
        NSTextField *appsLabel = [self createSummaryLabel:@"Selected Apps:" y:y bold:YES];
        appsLabel.tag = 206;
        [_summaryView addSubview:appsLabel];
        
        y -= 18;
        NSInteger appIndex = 0;
        for (NSString *bundleId in g_selected_apps) {
            // Find app name
            NSString *appName = bundleId;
            for (NSDictionary *app in g_available_apps) {
                if ([app[@"bundle_id"] isEqualToString:bundleId]) {
                    appName = app[@"name"];
                    break;
                }
            }
            NSTextField *appField = [[NSTextField alloc] initWithFrame:NSMakeRect(120, y, 300, 15)];
            appField.stringValue = [NSString stringWithFormat:@"• %@", appName];
            appField.bezeled = NO;
            appField.editable = NO;
            appField.drawsBackground = NO;
            appField.font = [NSFont systemFontOfSize:11];
            appField.textColor = [NSColor secondaryLabelColor];
            appField.tag = 250 + appIndex; // Unique tag using index
            [_summaryView addSubview:appField];
            y -= 15;
            appIndex++;
            if (y < 40) break; // Allow more apps to be shown
        }
    }
}

- (NSTextField *)createSummaryLabel:(NSString *)text y:(CGFloat)y bold:(BOOL)bold {
    NSTextField *field = [[NSTextField alloc] initWithFrame:NSMakeRect(100, y, 150, 18)];
    field.stringValue = text;
    field.bezeled = NO;
    field.editable = NO;
    field.drawsBackground = NO;
    field.font = bold ? [NSFont boldSystemFontOfSize:12] : [NSFont systemFontOfSize:12];
    return field;
}

- (NSTextField *)createSummaryValue:(NSString *)text y:(CGFloat)y {
    NSTextField *field = [[NSTextField alloc] initWithFrame:NSMakeRect(250, y, 200, 18)];
    field.stringValue = text;
    field.bezeled = NO;
    field.editable = NO;
    field.drawsBackground = NO;
    field.font = [NSFont systemFontOfSize:12];
    return field;
}

// ============================================================================
// Navigation
// ============================================================================

- (void)showStep:(NSInteger)step {
    // Remove current step view
    for (NSView *subview in [_contentView.subviews copy]) {
        [subview removeFromSuperview];
    }
    
    // Stop permission timer if leaving permissions step
    if (_currentStep == 1) {
        [self stopPermissionTimer];
    }
    
    _currentStep = step;
    
    // Add new step view
    NSView *stepView = nil;
    switch (step) {
        case 0: stepView = _welcomeView; break;
        case 1: stepView = _permissionsView; break;
        case 2: stepView = _appSelectionView; break;
        case 3: stepView = _autostartView; break;
        case 4: stepView = _summaryView; break;
    }
    
    if (stepView) {
        stepView.frame = _contentView.bounds;
        [_contentView addSubview:stepView];
    }
    
    // Update buttons
    _backButton.hidden = (step == 0);
    
    // Reset button color
    if (@available(macOS 11.0, *)) {
        _nextButton.bezelColor = nil;
    }
    
    if (step == 4) {
        _nextButton.title = @"Finish";
        [self updateSummaryView];
    } else if (step == 1) {
        // Permissions step - button text depends on permission state
        [self updateNextButtonForPermissions];
    } else {
        _nextButton.title = @"Next";
    }
    
    // Update step label
    _stepLabel.stringValue = [NSString stringWithFormat:@"Step %ld of 5", (long)(step + 1)];
    
    // Start permission timer if on permissions step
    if (step == 1) {
        [self startPermissionTimer];
    }
    
    // Reload table if on app selection step
    if (step == 2) {
        [_appTableView reloadData];
    }
}

- (void)backClicked:(id)sender {
    if (_currentStep > 0) {
        [self showStep:_currentStep - 1];
    }
}

- (void)nextClicked:(id)sender {
    // On permissions step, if not all required permissions granted, restart app
    if (_currentStep == 1 && !_allPermissionsGranted) {
        [self restartApp];
        return;
    }
    
    if (_currentStep < 4) {
        [self showStep:_currentStep + 1];
    } else {
        // Finish - save config and close
        [self finishWizard];
    }
}

- (void)stopModalNow {
    [NSApp stopModal];
}

- (void)cancelClicked:(id)sender {
    g_config->cancelled = YES;
    g_config->completed = NO;
    [self stopPermissionTimer];
    
    // Close window immediately so user sees feedback
    [self.window orderOut:nil];
    
    // Stop modal after a tiny delay to let the window hide
    [self performSelector:@selector(stopModalNow) withObject:nil afterDelay:0.01];
}

- (void)finishWizard {
    g_config->capture_all = (_captureAllCheckbox.state == NSControlStateValueOn);
    g_config->enable_autostart = (_autostartCheckbox.state == NSControlStateValueOn);
    g_config->completed = YES;
    g_config->cancelled = NO;
    
    // Copy selected apps
    if (g_selected_apps.count > 0 && !g_config->capture_all) {
        NSArray *apps = [g_selected_apps allObjects];
        const char **appArray = (const char **)malloc(sizeof(char *) * apps.count);
        for (NSUInteger i = 0; i < apps.count; i++) {
            appArray[i] = strdup([apps[i] UTF8String]);
        }
        g_config->selected_apps = appArray;
        g_config->selected_apps_count = apps.count;
    } else {
        g_config->selected_apps = NULL;
        g_config->selected_apps_count = 0;
    }
    
    [self stopPermissionTimer];
    
    // Close window immediately so user sees feedback
    [self.window orderOut:nil];
    
    // Stop modal after a tiny delay to let the window hide
    [self performSelector:@selector(stopModalNow) withObject:nil afterDelay:0.01];
}

- (void)windowWillClose:(NSNotification *)notification {
    // Only handle if not already processed by finish/cancel
    if (!g_config->completed && !g_config->cancelled) {
        g_config->cancelled = YES;
        [self stopPermissionTimer];
        [NSApp stopModal];
    }
}

@end

// ============================================================================
// C Interface Implementation
// ============================================================================

void wizard_set_apps(const WizardAppInfo *apps, size_t count) {
    if (!g_available_apps) {
        g_available_apps = [[NSMutableArray alloc] init];
    }
    [g_available_apps removeAllObjects];
    
    if (!g_selected_apps) {
        g_selected_apps = [[NSMutableSet alloc] init];
    }
    [g_selected_apps removeAllObjects];
    
    for (size_t i = 0; i < count; i++) {
        NSDictionary *app = @{
            @"bundle_id": apps[i].bundle_id ? [NSString stringWithUTF8String:apps[i].bundle_id] : @"",
            @"name": apps[i].name ? [NSString stringWithUTF8String:apps[i].name] : @"Unknown",
            @"pid": @(apps[i].pid)
        };
        [g_available_apps addObject:app];
    }
}

int wizard_run(WizardConfig *config) {
    if (g_wizard_running) {
        return -1;
    }
    
    g_wizard_running = YES;
    g_config = config;
    
    // Initialize config
    config->capture_all = false;
    config->enable_autostart = true;
    config->selected_apps = NULL;
    config->selected_apps_count = 0;
    config->completed = false;
    config->cancelled = false;
    
    @autoreleasepool {
        // Ensure NSApplication is initialized
        NSApplication *app = [NSApplication sharedApplication];
        [app setActivationPolicy:NSApplicationActivationPolicyRegular];
        
        // Create and show wizard
        WizardWindowController *wizard = [[WizardWindowController alloc] init];
        
        // Set up window delegate to handle close
        [[NSNotificationCenter defaultCenter] addObserver:wizard
                                                 selector:@selector(windowWillClose:)
                                                     name:NSWindowWillCloseNotification
                                                   object:wizard.window];
        
        [wizard.window makeKeyAndOrderFront:nil];
        [app activateIgnoringOtherApps:YES];
        
        // Run modal
        [app runModalForWindow:wizard.window];
        
        [[NSNotificationCenter defaultCenter] removeObserver:wizard];
    }
    
    g_wizard_running = NO;
    g_config = NULL;
    
    return config->completed ? 0 : -1;
}

void wizard_free_result(WizardConfig *config) {
    if (config->selected_apps) {
        for (size_t i = 0; i < config->selected_apps_count; i++) {
            free((void *)config->selected_apps[i]);
        }
        free((void *)config->selected_apps);
        config->selected_apps = NULL;
        config->selected_apps_count = 0;
    }
}

int wizard_check_accessibility(void) {
    return AXIsProcessTrusted() ? 1 : 0;
}

int wizard_check_screen_recording(void) {
    return CGPreflightScreenCaptureAccess() ? 1 : 0;
}

int wizard_request_accessibility(void) {
    NSDictionary *options = @{(__bridge NSString *)kAXTrustedCheckOptionPrompt: @YES};
    return AXIsProcessTrustedWithOptions((__bridge CFDictionaryRef)options) ? 1 : 0;
}

int wizard_request_screen_recording(void) {
    return CGRequestScreenCaptureAccess() ? 1 : 0;
}

void wizard_open_accessibility_settings(void) {
    [[NSWorkspace sharedWorkspace] openURL:[NSURL URLWithString:@"x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"]];
}

void wizard_open_screen_recording_settings(void) {
    [[NSWorkspace sharedWorkspace] openURL:[NSURL URLWithString:@"x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"]];
}

int wizard_check_notifications(void) {
    __block int result = 0;
    dispatch_semaphore_t semaphore = dispatch_semaphore_create(0);
    
    UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
    [center getNotificationSettingsWithCompletionHandler:^(UNNotificationSettings * _Nonnull settings) {
        if (settings.authorizationStatus == UNAuthorizationStatusAuthorized) {
            result = 1;
        }
        dispatch_semaphore_signal(semaphore);
    }];
    
    dispatch_semaphore_wait(semaphore, dispatch_time(DISPATCH_TIME_NOW, 2 * NSEC_PER_SEC));
    return result;
}

void wizard_request_notifications(void) {
    UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
    [center requestAuthorizationWithOptions:(UNAuthorizationOptionAlert | UNAuthorizationOptionSound)
                          completionHandler:^(BOOL granted, NSError * _Nullable error) {
        if (granted) {
            NSLog(@"[CrowdCast Wizard] Notification permission granted");
        } else {
            NSLog(@"[CrowdCast Wizard] Notification permission denied: %@", error);
        }
    }];
}

void wizard_open_notifications_settings(void) {
    // Open System Preferences to Notifications settings for this app
    NSString *bundleId = [[NSBundle mainBundle] bundleIdentifier];
    if (bundleId) {
        // Try to open notifications settings directly
        NSURL *url = [NSURL URLWithString:[NSString stringWithFormat:@"x-apple.systempreferences:com.apple.preference.notifications?id=%@", bundleId]];
        [[NSWorkspace sharedWorkspace] openURL:url];
    } else {
        // Fallback to general notifications pane
        [[NSWorkspace sharedWorkspace] openURL:[NSURL URLWithString:@"x-apple.systempreferences:com.apple.preference.notifications"]];
    }
}
