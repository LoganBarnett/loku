module Page.Browse exposing (Model, Msg, init, update, view)

import Api exposing (DirListing, Entry(..))
import Html exposing (..)
import Html.Attributes exposing (..)
import Http
import Route


type Model
    = Loading String
    | Loaded DirListing
    | Failed String


type Msg
    = GotListing (Result Http.Error DirListing)


init : String -> ( Model, Cmd Msg )
init path =
    ( Loading path, Api.getBrowse path GotListing )


update : Msg -> Model -> ( Model, Cmd Msg )
update msg _ =
    case msg of
        GotListing (Ok listing) ->
            ( Loaded listing, Cmd.none )

        GotListing (Err err) ->
            ( Failed (httpErrorToString err), Cmd.none )


view : Model -> Html Msg
view model =
    case model of
        Loading path ->
            p [ style "padding" "1rem" ] [ text ("Loading " ++ path ++ "…") ]

        Failed err ->
            p [ style "padding" "1rem", style "color" "var(--color-error)" ]
                [ text ("Error: " ++ err) ]

        Loaded listing ->
            div []
                [ viewBreadcrumb listing.path
                , div [ style "padding" "0.5rem 1rem" ]
                    (List.map viewEntry listing.entries)
                ]


viewBreadcrumb : String -> Html Msg
viewBreadcrumb path =
    let
        parts =
            path |> String.split "/" |> List.filter (not << String.isEmpty)

        crumbs =
            List.indexedMap
                (\i part ->
                    let
                        crumbPath =
                            parts |> List.take (i + 1) |> String.join "/"
                    in
                    span []
                        [ text " / "
                        , a [ href (Route.toString (Route.Browse crumbPath)) ]
                            [ text part ]
                        ]
                )
                parts
    in
    nav [ style "padding" "0.75rem 1rem", style "background" "var(--color-surface)" ]
        (a [ href (Route.toString (Route.Browse "")) ] [ text "Home" ]
            :: crumbs
        )


viewEntry : Entry -> Html Msg
viewEntry entry =
    case entry of
        Directory { name, path } ->
            div [ style "padding" "0.5rem 0" ]
                [ a [ href (Route.toString (Route.Browse path)) ]
                    [ text ("📁 " ++ name) ]
                ]

        Video { name, path, thumbPath, title } ->
            div [ style "display" "inline-block", style "margin" "0.5rem" ]
                [ a [ href (Route.toString (Route.Player path)) ]
                    [ case thumbPath of
                        Just tp ->
                            img
                                [ src (Api.thumbUrl tp)
                                , alt name
                                , style "width" "200px"
                                , style "height" "112px"
                                , style "object-fit" "cover"
                                , style "display" "block"
                                ]
                                []

                        Nothing ->
                            div
                                [ style "width" "200px"
                                , style "height" "112px"
                                , style "background" "var(--color-placeholder)"
                                , style "display" "flex"
                                , style "align-items" "center"
                                , style "justify-content" "center"
                                ]
                                [ text "▶" ]
                    , div
                        [ style "max-width" "200px"
                        , style "overflow" "hidden"
                        , style "text-overflow" "ellipsis"
                        , style "white-space" "nowrap"
                        , style "font-size" "0.85rem"
                        , style "margin-top" "0.25rem"
                        ]
                        [ text (Maybe.withDefault name title) ]
                    ]
                ]


httpErrorToString : Http.Error -> String
httpErrorToString err =
    case err of
        Http.BadUrl url ->
            "Bad URL: " ++ url

        Http.Timeout ->
            "Request timed out"

        Http.NetworkError ->
            "Network error"

        Http.BadStatus status ->
            "Server error: " ++ String.fromInt status

        Http.BadBody body ->
            "Unexpected response: " ++ body
