module Api exposing
    ( ActiveRip
    , DerivationState(..)
    , DerivationStatus
    , DirEntry
    , DirListing
    , DiscKind(..)
    , DiscSetEntry
    , Drive
    , DriveMedia
    , Entry(..)
    , Error
    , IdentityCandidate
    , IdentityState(..)
    , Library
    , LibraryKind(..)
    , ResolvedIdentity
    , RipJob
    , RipPolicy(..)
    , RipState(..)
    , RipTrigger(..)
    , RipsOverview
    , SearchPage
    , TmdbKind(..)
    , UnresolvedDisc
    , UnresolvedDiscs
    , VideoItem
    , errorToString
    , getBrowse
    , getItem
    , getLibraries
    , getRips
    , getSearch
    , getUnresolvedDiscs
    , postIdentify
    , postIdentity
    , postMainTitle
    , postRip
    , thumbUrl
    , videoUrl
    )

import Http
import Json.Decode as D
import Json.Decode.Pipeline exposing (optional, required)
import Json.Encode as E
import Url


type alias Library =
    { name : String
    , kind : LibraryKind
    }


type LibraryKind
    = Downloads
    | Discs


type alias DirListing =
    { library : String
    , path : String
    , entries : List Entry
    }


type Entry
    = Directory DirEntry
    | Video VideoItem
    | DiscSet DiscSetEntry


type alias DirEntry =
    { name : String
    , path : String
    }


{-| A disc rip grouped into one item: the presumed main feature plus every
title on the disc. Per-disc directories also carry their path, year, poster
art, and identity state; legacy flat rips leave those absent.
-}
type alias DiscSetEntry =
    { discSet : String
    , displayTitle : String
    , main : VideoItem
    , titles : List VideoItem
    , path : Maybe String
    , year : Maybe Int
    , discId : Maybe String
    , posterPath : Maybe String
    , identityState : Maybe IdentityState
    }


{-| Where a disc stands on having a resolved TMDB identity. The server will
grow this set, so an unknown value is carried rather than failing the whole
listing decode.
-}
type IdentityState
    = IdentityUnresolved
    | IdentityNeedsConfirmation
    | IdentityResolved
    | IdentityFailed
    | IdentityUnknown String


{-| Where a video stands on having a browser-playable representation.
-}
type DerivationState
    = NotNeeded
    | Done
    | Processing
    | Pending
    | Failed
    | Unknown


type alias DerivationStatus =
    { state : DerivationState
    , error : Maybe String
    }


type alias VideoItem =
    { library : String
    , path : String
    , name : String
    , title : Maybe String
    , titleSource : Maybe String
    , durationSecs : Maybe Float
    , uploadDate : Maybe String
    , year : Maybe Int
    , description : Maybe String
    , channel : Maybe String
    , channelUrl : Maybe String
    , webpageUrl : Maybe String
    , viewCount : Maybe Int
    , genres : List String
    , thumbPath : Maybe String
    , compatPath : Maybe String
    , nativeType : Maybe String
    , derivation : DerivationStatus
    , discSet : Maybe String
    , discTitleIndex : Maybe Int
    }


type alias SearchPage =
    { total : Int
    , limit : Int
    , offset : Int
    , items : List VideoItem
    }



-- RIPS


{-| Everything the rips page shows about the drives: whether ripping is
configured at all, each drive's current disc and job, and recent history.
-}
type alias RipsOverview =
    { enabled : Bool
    , drives : List Drive
    , recent : List RipJob
    }


type alias Drive =
    { device : String
    , library : String
    , policy : RipPolicy
    , lastError : Maybe String
    , media : Maybe DriveMedia
    , active : Maybe ActiveRip
    }


{-| What udev reports about the disc currently in a drive. The disc id and
the existing rip path are only known when udev supplied a filesystem UUID;
other discs get their id from the scan.
-}
type alias DriveMedia =
    { label : Maybe String
    , discId : Maybe String
    , kind : DiscKind
    , existingPath : Maybe String
    }


type alias ActiveRip =
    { id : Int
    , state : RipState
    , progressPermille : Int
    , currentOp : Maybe String
    , totalOp : Maybe String
    , label : Maybe String
    , path : Maybe String
    , startedAt : Int
    }


type alias RipJob =
    { id : Int
    , device : String
    , library : String
    , path : Maybe String
    , label : Maybe String
    , discId : Maybe String
    , policy : RipPolicy
    , trigger : RipTrigger
    , state : RipState
    , progressPermille : Int
    , currentOp : Maybe String
    , startedAt : Int
    , finishedAt : Maybe Int
    , error : Maybe String
    }


{-| What to do when a disc that has already been ripped is inserted again.
-}
type RipPolicy
    = Skip
    | Rescan
    | Rerip


type RipTrigger
    = Auto
    | Manual
    | TriggerUnknown String


{-| Rip job states arrive inside a document polled every few seconds and the
server will grow the set, so an unknown value is carried rather than failing
the whole poll. The same holds for `RipTrigger` and `DiscKind`.
-}
type RipState
    = RipDetected
    | RipScanning
    | RipRipping
    | RipFinalizing
    | RipDone
    | RipSkipped
    | RipFailed
    | RipUnknown String


type DiscKind
    = Dvd
    | Bd
    | DiscKindUnknown String



-- IDENTITY


type alias UnresolvedDiscs =
    { identityEnabled : Bool
    , discs : List UnresolvedDisc
    }


{-| A ripped disc whose TMDB identity the server could not settle on its
own, with the query it tried and the candidates it found.
-}
type alias UnresolvedDisc =
    { library : String
    , path : String
    , label : String
    , discId : String
    , identityState : IdentityState
    , error : Maybe String
    , query : String
    , yearHint : Maybe Int
    , kindHint : Maybe TmdbKind
    , fingerprintStrong : Bool
    , attemptedAt : Maybe Int
    , candidates : List IdentityCandidate
    }


type alias IdentityCandidate =
    { tmdbKind : TmdbKind
    , tmdbId : Int
    , title : String
    , originalTitle : Maybe String
    , year : Maybe Int
    , overview : Maybe String
    , posterUrl : Maybe String
    , popularity : Float
    , voteCount : Int
    }


type TmdbKind
    = Movie
    | Tv


{-| The outcome of confirming an identity: the disc directory's new path
(renamed when the title could be applied) and any artwork that could not
be fetched.
-}
type alias ResolvedIdentity =
    { library : String
    , path : String
    , previousPath : String
    , title : String
    , year : Maybe Int
    , renamed : Bool
    , artworkWarnings : List String
    }



-- ERRORS


{-| Why a request failed. Unlike `Http.Error`, a bad status keeps the
response body: the server answers errors with a human-readable sentence
(for example why a rip was refused), and pages show it verbatim.
-}
type Error
    = BadUrl String
    | Timeout
    | NetworkError
    | BadStatus Int String
    | BadBody String


errorToString : Error -> String
errorToString err =
    case err of
        BadUrl url ->
            "Bad URL: " ++ url

        Timeout ->
            "Request timed out"

        NetworkError ->
            "Network error"

        BadStatus status body ->
            if String.isEmpty (String.trim body) then
                "Server error: " ++ String.fromInt status

            else
                String.trim body

        BadBody body ->
            "Unexpected response: " ++ body


expectJson : (Result Error a -> msg) -> D.Decoder a -> Http.Expect msg
expectJson toMsg decoder =
    Http.expectStringResponse toMsg
        (resolve
            (\body ->
                D.decodeString decoder body
                    |> Result.mapError D.errorToString
            )
        )


expectWhatever : (Result Error () -> msg) -> Http.Expect msg
expectWhatever toMsg =
    Http.expectStringResponse toMsg (resolve (\_ -> Ok ()))


resolve : (String -> Result String a) -> Http.Response String -> Result Error a
resolve fromBody response =
    case response of
        Http.BadUrl_ url ->
            Err (BadUrl url)

        Http.Timeout_ ->
            Err Timeout

        Http.NetworkError_ ->
            Err NetworkError

        Http.BadStatus_ metadata body ->
            Err (BadStatus metadata.statusCode body)

        Http.GoodStatus_ _ body ->
            fromBody body |> Result.mapError BadBody



-- REQUESTS


getLibraries : (Result Error (List Library) -> msg) -> Cmd msg
getLibraries toMsg =
    Http.get
        { url = "/api/libraries"
        , expect = expectJson toMsg (D.list libraryDecoder)
        }


getBrowse : String -> String -> (Result Error DirListing -> msg) -> Cmd msg
getBrowse library path toMsg =
    Http.get
        { url =
            "/api/browse?library="
                ++ Url.percentEncode library
                ++ "&path="
                ++ Url.percentEncode path
        , expect = expectJson toMsg dirListingDecoder
        }


getItem : String -> String -> (Result Error VideoItem -> msg) -> Cmd msg
getItem library path toMsg =
    Http.get
        { url =
            "/api/item?library="
                ++ Url.percentEncode library
                ++ "&path="
                ++ Url.percentEncode path
        , expect = expectJson toMsg videoItemDecoder
        }


getSearch :
    { query : String, library : Maybe String, limit : Int, offset : Int }
    -> (Result Error SearchPage -> msg)
    -> Cmd msg
getSearch params toMsg =
    Http.get
        { url =
            "/api/search?q="
                ++ Url.percentEncode params.query
                ++ (case params.library of
                        Just library ->
                            "&library=" ++ Url.percentEncode library

                        Nothing ->
                            ""
                   )
                ++ "&limit="
                ++ String.fromInt params.limit
                ++ "&offset="
                ++ String.fromInt params.offset
        , expect = expectJson toMsg searchPageDecoder
        }


postMainTitle :
    { library : String, discSet : String, path : String }
    -> (Result Error () -> msg)
    -> Cmd msg
postMainTitle params toMsg =
    Http.post
        { url = "/api/disc-sets/main"
        , body =
            Http.jsonBody
                (E.object
                    [ ( "library", E.string params.library )
                    , ( "disc_set", E.string params.discSet )
                    , ( "path", E.string params.path )
                    ]
                )
        , expect = expectWhatever toMsg
        }


getRips : (Result Error RipsOverview -> msg) -> Cmd msg
getRips toMsg =
    Http.get
        { url = "/api/rips"
        , expect = expectJson toMsg ripsOverviewDecoder
        }


{-| Ask the server to scan and rip the disc in a drive now. The server
answers with the new job, or refuses with a plain-text reason (no disc,
a rip already running, ripping disabled) that `errorToString` surfaces.
-}
postRip :
    { device : String, policy : RipPolicy }
    -> (Result Error RipJob -> msg)
    -> Cmd msg
postRip params toMsg =
    Http.post
        { url = "/api/rips"
        , body =
            Http.jsonBody
                (E.object
                    [ ( "device", E.string params.device )
                    , ( "policy", E.string (ripPolicyToString params.policy) )
                    ]
                )
        , expect = expectJson toMsg ripJobDecoder
        }


getUnresolvedDiscs : (Result Error UnresolvedDiscs -> msg) -> Cmd msg
getUnresolvedDiscs toMsg =
    Http.get
        { url = "/api/discs/unresolved"
        , expect = expectJson toMsg unresolvedDiscsDecoder
        }


{-| Run a fresh TMDB search for a disc without committing anything.
-}
postIdentify :
    { library : String, path : String, query : String, year : Maybe Int }
    -> (Result Error (List IdentityCandidate) -> msg)
    -> Cmd msg
postIdentify params toMsg =
    Http.post
        { url = "/api/discs/" ++ Url.percentEncode params.library ++ "/identify"
        , body =
            Http.jsonBody
                (E.object
                    [ ( "path", E.string params.path )
                    , ( "query", E.string params.query )
                    , ( "year"
                      , params.year |> Maybe.map E.int |> Maybe.withDefault E.null
                      )
                    ]
                )
        , expect =
            expectJson toMsg
                (D.field "candidates" (D.list identityCandidateDecoder))
        }


{-| Commit a TMDB identity for a disc: the server writes the NFO and
artwork and renames the directory.
-}
postIdentity :
    { library : String, path : String, tmdbKind : TmdbKind, tmdbId : Int }
    -> (Result Error ResolvedIdentity -> msg)
    -> Cmd msg
postIdentity params toMsg =
    Http.post
        { url = "/api/discs/" ++ Url.percentEncode params.library ++ "/identity"
        , body =
            Http.jsonBody
                (E.object
                    [ ( "path", E.string params.path )
                    , ( "tmdb_kind", E.string (tmdbKindToString params.tmdbKind) )
                    , ( "tmdb_id", E.int params.tmdbId )
                    ]
                )
        , expect = expectJson toMsg resolvedIdentityDecoder
        }


videoUrl : String -> String -> String
videoUrl library path =
    "/files/" ++ Url.percentEncode library ++ "/" ++ encodePath path


thumbUrl : String -> String -> String
thumbUrl library path =
    videoUrl library path


{-| Percent-encode each path segment individually, preserving slash separators.
-}
encodePath : String -> String
encodePath path =
    path
        |> String.split "/"
        |> List.map Url.percentEncode
        |> String.join "/"



-- DECODERS


libraryDecoder : D.Decoder Library
libraryDecoder =
    D.map2 Library
        (D.field "name" D.string)
        (D.field "kind" libraryKindDecoder)


libraryKindDecoder : D.Decoder LibraryKind
libraryKindDecoder =
    D.string
        |> D.andThen
            (\kind ->
                case kind of
                    "downloads" ->
                        D.succeed Downloads

                    "discs" ->
                        D.succeed Discs

                    _ ->
                        D.fail ("Unknown library kind: " ++ kind)
            )


dirListingDecoder : D.Decoder DirListing
dirListingDecoder =
    D.map3 DirListing
        (D.field "library" D.string)
        (D.field "path" D.string)
        (D.field "entries" (D.list entryDecoder))


entryDecoder : D.Decoder Entry
entryDecoder =
    D.field "type" D.string
        |> D.andThen
            (\entryType ->
                case entryType of
                    "directory" ->
                        D.map Directory dirEntryDecoder

                    "video" ->
                        D.map Video videoItemDecoder

                    "disc_set" ->
                        D.map DiscSet discSetDecoder

                    _ ->
                        D.fail ("Unknown entry type: " ++ entryType)
            )


dirEntryDecoder : D.Decoder DirEntry
dirEntryDecoder =
    D.map2 DirEntry
        (D.field "name" D.string)
        (D.field "path" D.string)


discSetDecoder : D.Decoder DiscSetEntry
discSetDecoder =
    D.succeed DiscSetEntry
        |> required "disc_set" D.string
        |> required "display_title" D.string
        |> required "main" videoItemDecoder
        |> required "titles" (D.list videoItemDecoder)
        |> optional "path" (D.map Just D.string) Nothing
        |> optional "year" (D.map Just D.int) Nothing
        |> optional "disc_id" (D.map Just D.string) Nothing
        |> optional "poster_path" (D.map Just D.string) Nothing
        |> optional "identity_state" (D.map Just identityStateDecoder) Nothing


identityStateDecoder : D.Decoder IdentityState
identityStateDecoder =
    D.string
        |> D.map
            (\state ->
                case state of
                    "unresolved" ->
                        IdentityUnresolved

                    "needs_confirmation" ->
                        IdentityNeedsConfirmation

                    "resolved" ->
                        IdentityResolved

                    "failed" ->
                        IdentityFailed

                    other ->
                        IdentityUnknown other
            )


videoItemDecoder : D.Decoder VideoItem
videoItemDecoder =
    D.succeed VideoItem
        |> required "library" D.string
        |> required "path" D.string
        |> required "name" D.string
        |> optional "title" (D.map Just D.string) Nothing
        |> optional "title_source" (D.map Just D.string) Nothing
        |> optional "duration_secs" (D.map Just D.float) Nothing
        |> optional "upload_date" (D.map Just D.string) Nothing
        |> optional "year" (D.map Just D.int) Nothing
        |> optional "description" (D.map Just D.string) Nothing
        |> optional "channel" (D.map Just D.string) Nothing
        |> optional "channel_url" (D.map Just D.string) Nothing
        |> optional "webpage_url" (D.map Just D.string) Nothing
        |> optional "view_count" (D.map Just D.int) Nothing
        |> optional "genres" (D.list D.string) []
        |> optional "thumb_path" (D.map Just D.string) Nothing
        |> optional "compat_path" (D.map Just D.string) Nothing
        |> optional "native_type" (D.map Just D.string) Nothing
        |> required "derivation" derivationStatusDecoder
        |> optional "disc_set" (D.map Just D.string) Nothing
        |> optional "disc_title_index" (D.map Just D.int) Nothing


derivationStatusDecoder : D.Decoder DerivationStatus
derivationStatusDecoder =
    D.map2 DerivationStatus
        (D.field "state" derivationStateDecoder)
        (D.maybe (D.field "error" D.string))


derivationStateDecoder : D.Decoder DerivationState
derivationStateDecoder =
    D.string
        |> D.andThen
            (\state ->
                case state of
                    "not_needed" ->
                        D.succeed NotNeeded

                    "done" ->
                        D.succeed Done

                    "processing" ->
                        D.succeed Processing

                    "pending" ->
                        D.succeed Pending

                    "failed" ->
                        D.succeed Failed

                    "unknown" ->
                        D.succeed Unknown

                    _ ->
                        D.fail ("Unknown derivation state: " ++ state)
            )


searchPageDecoder : D.Decoder SearchPage
searchPageDecoder =
    D.map4 SearchPage
        (D.field "total" D.int)
        (D.field "limit" D.int)
        (D.field "offset" D.int)
        (D.field "items" (D.list videoItemDecoder))


ripsOverviewDecoder : D.Decoder RipsOverview
ripsOverviewDecoder =
    D.map3 RipsOverview
        (D.field "enabled" D.bool)
        (D.field "drives" (D.list driveDecoder))
        (D.field "recent" (D.list ripJobDecoder))


driveDecoder : D.Decoder Drive
driveDecoder =
    D.succeed Drive
        |> required "device" D.string
        |> required "library" D.string
        |> required "policy" ripPolicyDecoder
        |> optional "last_error" (D.map Just D.string) Nothing
        |> optional "media" (D.map Just driveMediaDecoder) Nothing
        |> optional "active" (D.map Just activeRipDecoder) Nothing


driveMediaDecoder : D.Decoder DriveMedia
driveMediaDecoder =
    D.succeed DriveMedia
        |> optional "label" (D.map Just D.string) Nothing
        |> optional "disc_id" (D.map Just D.string) Nothing
        |> required "kind" discKindDecoder
        |> optional "existing_path" (D.map Just D.string) Nothing


activeRipDecoder : D.Decoder ActiveRip
activeRipDecoder =
    D.succeed ActiveRip
        |> required "id" D.int
        |> required "state" ripStateDecoder
        |> required "progress_permille" D.int
        |> optional "current_op" (D.map Just D.string) Nothing
        |> optional "total_op" (D.map Just D.string) Nothing
        |> optional "label" (D.map Just D.string) Nothing
        |> optional "path" (D.map Just D.string) Nothing
        |> required "started_at" D.int


ripJobDecoder : D.Decoder RipJob
ripJobDecoder =
    D.succeed RipJob
        |> required "id" D.int
        |> required "device" D.string
        |> required "library" D.string
        |> optional "path" (D.map Just D.string) Nothing
        |> optional "label" (D.map Just D.string) Nothing
        |> optional "disc_id" (D.map Just D.string) Nothing
        |> required "policy" ripPolicyDecoder
        |> required "trigger" ripTriggerDecoder
        |> required "state" ripStateDecoder
        |> required "progress_permille" D.int
        |> optional "current_op" (D.map Just D.string) Nothing
        |> required "started_at" D.int
        |> optional "finished_at" (D.map Just D.int) Nothing
        |> optional "error" (D.map Just D.string) Nothing


ripPolicyDecoder : D.Decoder RipPolicy
ripPolicyDecoder =
    D.string
        |> D.andThen
            (\policy ->
                case policy of
                    "skip" ->
                        D.succeed Skip

                    "rescan" ->
                        D.succeed Rescan

                    "rerip" ->
                        D.succeed Rerip

                    _ ->
                        D.fail ("Unknown rip policy: " ++ policy)
            )


ripPolicyToString : RipPolicy -> String
ripPolicyToString policy =
    case policy of
        Skip ->
            "skip"

        Rescan ->
            "rescan"

        Rerip ->
            "rerip"


ripTriggerDecoder : D.Decoder RipTrigger
ripTriggerDecoder =
    D.string
        |> D.map
            (\trigger ->
                case trigger of
                    "auto" ->
                        Auto

                    "manual" ->
                        Manual

                    other ->
                        TriggerUnknown other
            )


ripStateDecoder : D.Decoder RipState
ripStateDecoder =
    D.string
        |> D.map
            (\state ->
                case state of
                    "detected" ->
                        RipDetected

                    "scanning" ->
                        RipScanning

                    "ripping" ->
                        RipRipping

                    "finalizing" ->
                        RipFinalizing

                    "done" ->
                        RipDone

                    "skipped" ->
                        RipSkipped

                    "failed" ->
                        RipFailed

                    other ->
                        RipUnknown other
            )


discKindDecoder : D.Decoder DiscKind
discKindDecoder =
    D.string
        |> D.map
            (\kind ->
                case kind of
                    "dvd" ->
                        Dvd

                    "bd" ->
                        Bd

                    other ->
                        DiscKindUnknown other
            )


unresolvedDiscsDecoder : D.Decoder UnresolvedDiscs
unresolvedDiscsDecoder =
    D.map2 UnresolvedDiscs
        (D.field "identity_enabled" D.bool)
        (D.field "discs" (D.list unresolvedDiscDecoder))


unresolvedDiscDecoder : D.Decoder UnresolvedDisc
unresolvedDiscDecoder =
    D.succeed UnresolvedDisc
        |> required "library" D.string
        |> required "path" D.string
        |> required "label" D.string
        |> required "disc_id" D.string
        |> required "identity_state" identityStateDecoder
        |> optional "error" (D.map Just D.string) Nothing
        |> required "query" D.string
        |> optional "year_hint" (D.map Just D.int) Nothing
        |> optional "kind_hint" (D.map Just tmdbKindDecoder) Nothing
        |> required "fingerprint_strong" D.bool
        |> optional "attempted_at" (D.map Just D.int) Nothing
        |> optional "candidates" (D.list identityCandidateDecoder) []


identityCandidateDecoder : D.Decoder IdentityCandidate
identityCandidateDecoder =
    D.succeed IdentityCandidate
        |> required "tmdb_kind" tmdbKindDecoder
        |> required "tmdb_id" D.int
        |> required "title" D.string
        |> optional "original_title" (D.map Just D.string) Nothing
        |> optional "year" (D.map Just D.int) Nothing
        |> optional "overview" (D.map Just D.string) Nothing
        |> optional "poster_url" (D.map Just D.string) Nothing
        |> required "popularity" D.float
        |> required "vote_count" D.int


tmdbKindDecoder : D.Decoder TmdbKind
tmdbKindDecoder =
    D.string
        |> D.andThen
            (\kind ->
                case kind of
                    "movie" ->
                        D.succeed Movie

                    "tv" ->
                        D.succeed Tv

                    _ ->
                        D.fail ("Unknown TMDB kind: " ++ kind)
            )


tmdbKindToString : TmdbKind -> String
tmdbKindToString kind =
    case kind of
        Movie ->
            "movie"

        Tv ->
            "tv"


resolvedIdentityDecoder : D.Decoder ResolvedIdentity
resolvedIdentityDecoder =
    D.succeed ResolvedIdentity
        |> required "library" D.string
        |> required "path" D.string
        |> required "previous_path" D.string
        |> required "title" D.string
        |> optional "year" (D.map Just D.int) Nothing
        |> required "renamed" D.bool
        |> optional "artwork_warnings" (D.list D.string) []
