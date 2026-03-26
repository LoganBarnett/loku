module Api exposing
    ( DirListing
    , Entry(..)
    , DirEntry
    , VideoEntry
    , getBrowse
    , videoUrl
    , thumbUrl
    )

import Http
import Json.Decode as D
import Url


type alias DirListing =
    { path : String
    , entries : List Entry
    }


type Entry
    = Directory DirEntry
    | Video VideoEntry


type alias DirEntry =
    { name : String
    , path : String
    }


type alias VideoEntry =
    { name : String
    , path : String
    , thumbPath : Maybe String
    , title : Maybe String
    , durationSecs : Maybe Float
    , uploadDate : Maybe String
    }


getBrowse : String -> (Result Http.Error DirListing -> msg) -> Cmd msg
getBrowse path toMsg =
    Http.get
        { url = "/api/browse?path=" ++ Url.percentEncode path
        , expect = Http.expectJson toMsg dirListingDecoder
        }


videoUrl : String -> String
videoUrl path =
    "/files/" ++ path


thumbUrl : String -> String
thumbUrl path =
    "/files/" ++ path


dirListingDecoder : D.Decoder DirListing
dirListingDecoder =
    D.map2 DirListing
        (D.field "path" D.string)
        (D.field "entries" (D.list entryDecoder))


entryDecoder : D.Decoder Entry
entryDecoder =
    D.field "type" D.string
        |> D.andThen
            (\t ->
                case t of
                    "directory" ->
                        D.map2
                            (\name path -> Directory { name = name, path = path })
                            (D.field "name" D.string)
                            (D.field "path" D.string)

                    "video" ->
                        D.map6
                            (\name path thumbPath title durationSecs uploadDate ->
                                Video
                                    { name = name
                                    , path = path
                                    , thumbPath = thumbPath
                                    , title = title
                                    , durationSecs = durationSecs
                                    , uploadDate = uploadDate
                                    }
                            )
                            (D.field "name" D.string)
                            (D.field "path" D.string)
                            (D.maybe (D.field "thumb_path" D.string))
                            (D.maybe (D.field "title" D.string))
                            (D.maybe (D.field "duration_secs" D.float))
                            (D.maybe (D.field "upload_date" D.string))

                    _ ->
                        D.fail ("Unknown entry type: " ++ t)
            )
