// Copyright (c) 2025 Jonas van den Berg
// This file is licensed under the BSD 3-Clause License.

#include "private/MediaRemote.h"
#include <limits.h>

#import <Foundation/Foundation.h>

#import "MediaRemoteAdapter.h"
#import "adapter/env.h"
#import "adapter/globals.h"
#import "adapter/now_playing.h"
#import "utility/helpers.h"

static NSArray<NSNumber *> *acceptedCommands;

__attribute__((constructor)) static void init() {
    acceptedCommands = @[
        @(kMRAPlay),
        @(kMRAPause),
        @(kMRATogglePlayPause),
        @(kMRAStop),
        @(kMRANextTrack),
        @(kMRAPreviousTrack),
        @(kMRAToggleShuffle),
        @(kMRAToggleRepeat),
        @(kMRAStartForwardSeek),
        @(kMRAEndForwardSeek),
        @(kMRAStartBackwardSeek),
        @(kMRAEndBackwardSeek),
        @(kMRAGoBackFifteenSeconds),
        @(kMRASkipFifteenSeconds),
        @(kMRALikeTrack),
        @(kMRABanTrack),
    ];
}

static MRCommand findCommand(int command, bool *found) {
    if ([acceptedCommands containsObject:@(command)]) {
        *found = true;
        return (MRCommand)command;
    }
    *found = false;
    return (MRCommand)0;
}

static NSDictionary *ratingOptions(void) {
    __block NSDictionary *options = nil;
    dispatch_semaphore_t semaphore = dispatch_semaphore_create(0);
    g_mediaRemote.getNowPlayingInfo(
        dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0),
        ^(NSDictionary *information) {
          NSMutableDictionary *values = [NSMutableDictionary dictionary];
          id trackID = information[kMRMediaRemoteNowPlayingInfoUniqueIdentifier]
              ?: information[kMRMediaRemoteNowPlayingInfoContentItemIdentifier];
          id stationID = information[kMRMediaRemoteNowPlayingInfoRadioStationIdentifier];
          id stationHash = information[kMRMediaRemoteNowPlayingInfoRadioStationHash];
          if (trackID && trackID != [NSNull null])
              values[kMRMediaRemoteOptionTrackID] = trackID;
          if (stationID && stationID != [NSNull null])
              values[kMRMediaRemoteOptionStationID] = stationID;
          if (stationHash && stationHash != [NSNull null])
              values[kMRMediaRemoteOptionStationHash] = stationHash;
          options = [values copy];
          dispatch_semaphore_signal(semaphore);
        });
    dispatch_semaphore_wait(
        semaphore, dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC));
    return options;
}

void adapter_send(MRACommand command) {

    bool ok = false;
    MRCommand commandValue = findCommand((int)command, &ok);
    if (!ok) {
        failf(@"Invalid command: %d", command);
    }

    bool ratingCommand =
        commandValue == kMRLikeTrack || commandValue == kMRBanTrack;
    NSDictionary *options = ratingCommand ? ratingOptions() : nil;
    if (ratingCommand && options.count != 3) {
        failf(@"Missing now-playing identifiers for rating command %d", command);
    }
    bool result = g_mediaRemote.sendCommand(commandValue, options);
    if (!result) {
        failf(@"Failed to send command %d", command);
    }

    waitForCommandCompletion();
}

static inline int send_0_command() {
    return getEnvFuncParamIntSafe(@"adapter_send", 0, @"command");
}

void adapter_send_env() { adapter_send((MRACommand)send_0_command()); }
