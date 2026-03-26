module Route exposing (Route(..), BrowseParams, parse, toString)

import Url exposing (Url)


type alias BrowseParams =
    { path : String
    , query : String
    , page : Int
    }


type Route
    = Browse BrowseParams
    | Player String
    | NotFound


{-| Derive a Route from a URL.

    /browse/          → Browse { path = "", query = "", page = 1 }
    /browse/My%20Show → Browse { path = "My Show", query = "", page = 1 }
    /player/foo/bar   → Player "foo/bar"

-}
parse : Url -> Route
parse url =
    let
        qp =
            parseQueryParams url.query
    in
    if String.startsWith "/browse/" url.path then
        Browse
            { path = String.dropLeft 8 url.path |> percentDecode
            , query = qp.query
            , page = qp.page
            }

    else if url.path == "/browse" then
        Browse { path = "", query = qp.query, page = qp.page }

    else if url.path == "/" || url.path == "" then
        Browse { path = "", query = "", page = 1 }

    else if String.startsWith "/player/" url.path then
        Player (String.dropLeft 8 url.path |> percentDecode)

    else
        NotFound


toString : Route -> String
toString route =
    case route of
        Browse { path, query, page } ->
            let
                base =
                    if String.isEmpty path then
                        "/browse/"

                    else
                        "/browse/" ++ encodePath path

                qp =
                    (if String.isEmpty query then
                        []

                     else
                        [ "q=" ++ Url.percentEncode query ]
                    )
                        ++ (if page <= 1 then
                                []

                            else
                                [ "page=" ++ String.fromInt page ]
                           )
            in
            case qp of
                [] ->
                    base

                _ ->
                    base ++ "?" ++ String.join "&" qp

        Player path ->
            "/player/" ++ encodePath path

        NotFound ->
            "/"


percentDecode : String -> String
percentDecode s =
    Url.percentDecode s |> Maybe.withDefault s


{-| Encode each path segment individually, preserving slash separators. -}
encodePath : String -> String
encodePath path =
    path
        |> String.split "/"
        |> List.map Url.percentEncode
        |> String.join "/"


parseQueryParams : Maybe String -> { query : String, page : Int }
parseQueryParams maybeQs =
    case maybeQs of
        Nothing ->
            { query = "", page = 1 }

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

                p =
                    get "page" |> Maybe.andThen String.toInt |> Maybe.withDefault 1 |> max 1
            in
            { query = q, page = p }


splitKeyValue : String -> Maybe ( String, String )
splitKeyValue s =
    case String.split "=" s of
        k :: rest ->
            Just ( k, String.join "=" rest )

        _ ->
            Nothing
