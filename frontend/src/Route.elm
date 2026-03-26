module Route exposing (Route(..), parse, toString)

import Url exposing (Url)


type Route
    = Browse String
    | Player String
    | NotFound


{-| Derive a Route from a URL.

    /browse/          → Browse ""
    /browse/My%20Show → Browse "My Show"
    /player/foo/bar   → Player "foo/bar"

-}
parse : Url -> Route
parse url =
    if String.startsWith "/browse/" url.path then
        Browse (String.dropLeft 8 url.path |> percentDecode)

    else if url.path == "/browse" then
        Browse ""

    else if url.path == "/" || url.path == "" then
        Browse ""

    else if String.startsWith "/player/" url.path then
        Player (String.dropLeft 8 url.path |> percentDecode)

    else
        NotFound


toString : Route -> String
toString route =
    case route of
        Browse path ->
            if String.isEmpty path then
                "/browse/"

            else
                "/browse/" ++ encodePath path

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
