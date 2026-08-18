module Route exposing
    ( BrowseParams
    , PlayerParams
    , Route(..)
    , SearchParams
    , parse
    , toString
    )

import Url exposing (Url)


type alias BrowseParams =
    { library : String
    , path : String
    , page : Int
    }


type alias SearchParams =
    { query : String
    , library : Maybe String
    , page : Int
    }


type alias PlayerParams =
    { library : String
    , path : String
    }


type Route
    = Home
    | Browse BrowseParams
    | Search SearchParams
    | Player PlayerParams
    | NotFound


{-| Derive a Route from a URL.

    /                        → Home
    /browse/discs            → Browse { library = "discs", path = "" }
    /browse/discs/Movies     → Browse { library = "discs", path = "Movies" }
    /search?q=matrix         → Search { query = "matrix" }
    /player/discs/foo.mkv    → Player { library = "discs", path = "foo.mkv" }

-}
parse : Url -> Route
parse url =
    let
        qp =
            parseQueryParams url.query
    in
    if url.path == "/" || url.path == "" then
        Home

    else if String.startsWith "/browse/" url.path then
        let
            ( library, path ) =
                splitFirstSegment (String.dropLeft 8 url.path)
        in
        if String.isEmpty library then
            Home

        else
            Browse
                { library = percentDecode library
                , path = percentDecode path
                , page = qp.page
                }

    else if String.startsWith "/search" url.path then
        Search { query = qp.query, library = qp.library, page = qp.page }

    else if String.startsWith "/player/" url.path then
        let
            ( library, path ) =
                splitFirstSegment (String.dropLeft 8 url.path)
        in
        if String.isEmpty library || String.isEmpty path then
            NotFound

        else
            Player
                { library = percentDecode library
                , path = percentDecode path
                }

    else
        NotFound


toString : Route -> String
toString route =
    case route of
        Home ->
            "/"

        Browse { library, path, page } ->
            let
                base =
                    "/browse/"
                        ++ Url.percentEncode library
                        ++ (if String.isEmpty path then
                                ""

                            else
                                "/" ++ encodePath path
                           )
            in
            if page <= 1 then
                base

            else
                base ++ "?page=" ++ String.fromInt page

        Search { query, library, page } ->
            "/search?q="
                ++ Url.percentEncode query
                ++ (case library of
                        Just lib ->
                            "&library=" ++ Url.percentEncode lib

                        Nothing ->
                            ""
                   )
                ++ (if page <= 1 then
                        ""

                    else
                        "&page=" ++ String.fromInt page
                   )

        Player { library, path } ->
            "/player/" ++ Url.percentEncode library ++ "/" ++ encodePath path

        NotFound ->
            "/"


{-| Split off the first path segment (before any decoding, so encoded
slashes inside a segment stay put).
-}
splitFirstSegment : String -> ( String, String )
splitFirstSegment s =
    case String.split "/" s of
        first :: rest ->
            ( first, String.join "/" rest )

        [] ->
            ( "", "" )


percentDecode : String -> String
percentDecode s =
    Url.percentDecode s |> Maybe.withDefault s


{-| Encode each path segment individually, preserving slash separators.
-}
encodePath : String -> String
encodePath path =
    path
        |> String.split "/"
        |> List.map Url.percentEncode
        |> String.join "/"


parseQueryParams :
    Maybe String
    -> { query : String, library : Maybe String, page : Int }
parseQueryParams maybeQs =
    case maybeQs of
        Nothing ->
            { query = "", library = Nothing, page = 1 }

        Just qs ->
            let
                pairs =
                    qs
                        |> String.split "&"
                        |> List.filterMap splitKeyValue

                get key =
                    pairs
                        |> List.filterMap
                            (\( k, v ) ->
                                if k == key then
                                    Just v

                                else
                                    Nothing
                            )
                        |> List.head

                q =
                    get "q" |> Maybe.map percentDecode |> Maybe.withDefault ""

                lib =
                    get "library" |> Maybe.map percentDecode

                p =
                    get "page"
                        |> Maybe.andThen String.toInt
                        |> Maybe.withDefault 1
                        |> max 1
            in
            { query = q, library = lib, page = p }


splitKeyValue : String -> Maybe ( String, String )
splitKeyValue s =
    case String.split "=" s of
        k :: rest ->
            Just ( k, String.join "=" rest )

        _ ->
            Nothing
