module Api exposing
    ( DerivationState(..)
    , DerivationStatus
    , DirEntry
    , DirListing
    , DiscSetEntry
    , Entry(..)
    , Library
    , LibraryKind(..)
    , SearchPage
    , VideoItem
    , getBrowse
    , getItem
    , getLibraries
    , getSearch
    , postMainTitle
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


{-| A multi-title disc rip grouped into one item: the presumed main feature
plus every title on the disc.
-}
type alias DiscSetEntry =
    { discSet : String
    , displayTitle : String
    , main : VideoItem
    , titles : List VideoItem
    }


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



-- REQUESTS


getLibraries : (Result Http.Error (List Library) -> msg) -> Cmd msg
getLibraries toMsg =
    Http.get
        { url = "/api/libraries"
        , expect = Http.expectJson toMsg (D.list libraryDecoder)
        }


getBrowse : String -> String -> (Result Http.Error DirListing -> msg) -> Cmd msg
getBrowse library path toMsg =
    Http.get
        { url =
            "/api/browse?library="
                ++ Url.percentEncode library
                ++ "&path="
                ++ Url.percentEncode path
        , expect = Http.expectJson toMsg dirListingDecoder
        }


getItem : String -> String -> (Result Http.Error VideoItem -> msg) -> Cmd msg
getItem library path toMsg =
    Http.get
        { url =
            "/api/item?library="
                ++ Url.percentEncode library
                ++ "&path="
                ++ Url.percentEncode path
        , expect = Http.expectJson toMsg videoItemDecoder
        }


getSearch :
    { query : String, library : Maybe String, limit : Int, offset : Int }
    -> (Result Http.Error SearchPage -> msg)
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
        , expect = Http.expectJson toMsg searchPageDecoder
        }


postMainTitle :
    { library : String, discSet : String, path : String }
    -> (Result Http.Error () -> msg)
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
        , expect = Http.expectWhatever toMsg
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
    D.map4 DiscSetEntry
        (D.field "disc_set" D.string)
        (D.field "display_title" D.string)
        (D.field "main" videoItemDecoder)
        (D.field "titles" (D.list videoItemDecoder))


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
